//! The long-running loop: follow events, keep the state honest, enforce the budget.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use anyhow::Result;
use tracing::{debug, error, info, warn};

use crate::config::Config;
use crate::lru::{self, CONTAINER_USE_STATUSES, IMAGE_USE_STATUSES};
use crate::model::{bare_id, Event, ImageSummary};
use crate::podman::{Client, RemoveOutcome};
use crate::state::{self, State};

const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

pub struct Daemon {
    cfg: Config,
    client: Client,
    state: State,
    images: Vec<ImageSummary>,
}

impl Daemon {
    pub fn new(cfg: Config) -> Self {
        let client = Client::new(&cfg.socket);
        let state = State::load(&cfg.state);
        Daemon {
            cfg,
            client,
            state,
            images: Vec::new(),
        }
    }

    pub async fn run(mut self) -> Result<()> {
        info!(
            max_size = self.cfg.max_size,
            socket = %self.cfg.socket.display(),
            state = %self.cfg.state.display(),
            interval = ?self.cfg.interval,
            keep = self.cfg.keep.as_ref().map(|r| r.as_str()).unwrap_or("-"),
            dry_run = self.cfg.dry_run,
            known_images = self.state.last_used.len(),
            "podman-lru starting"
        );
        self.wait_for_podman().await;
        self.reconcile().await;
        self.enforce().await;
        self.save();

        let mut ticker = tokio::time::interval(self.cfg.interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await; // first tick fires immediately; the startup pass covered it
        let mut shutdown = std::pin::pin!(shutdown_signal());
        let mut backoff = BACKOFF_MIN;
        let mut stream = None;

        loop {
            if stream.is_none() {
                let since = self.state.last_event.map(|t| t.saturating_sub(1));
                match self.client.events(since).await {
                    Ok(s) => {
                        info!(since = ?since, "following podman events");
                        backoff = BACKOFF_MIN;
                        stream = Some(s);
                    }
                    Err(err) => {
                        warn!(err = %format!("{err:#}"), retry_in = ?backoff, "cannot open event stream");
                        tokio::select! {
                            _ = tokio::time::sleep(backoff) => {}
                            _ = &mut shutdown => break,
                        }
                        backoff = (backoff * 2).min(BACKOFF_MAX);
                        continue;
                    }
                }
            }
            let Some(current) = stream.as_mut() else {
                continue;
            };
            tokio::select! {
                next = current.next() => match next {
                    Ok(Some(event)) => self.handle_event(event).await,
                    Ok(None) => {
                        warn!("event stream closed by podman, reconnecting");
                        stream = None;
                        self.reconcile().await;
                        self.enforce().await;
                        self.save();
                    }
                    Err(err) => {
                        warn!(err = %format!("{err:#}"), "event stream failed, reconnecting");
                        stream = None;
                        self.reconcile().await;
                        self.enforce().await;
                        self.save();
                    }
                },
                _ = ticker.tick() => {
                    debug!("periodic reconcile");
                    self.reconcile().await;
                    self.enforce().await;
                    self.save();
                }
                _ = &mut shutdown => break,
            }
        }
        info!("shutting down");
        self.save();
        Ok(())
    }

    /// Podman may still be starting when we are; do not spam errors.
    async fn wait_for_podman(&self) {
        let mut backoff = BACKOFF_MIN;
        loop {
            match self.client.df().await {
                Ok(_) => return,
                Err(err) => {
                    warn!(err = %format!("{err:#}"), retry_in = ?backoff, "podman API not reachable yet");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(BACKOFF_MAX);
                }
            }
        }
    }

    async fn handle_event(&mut self, event: Event) {
        let now = state::now();
        let status = event.status.as_str();
        match event.kind.as_str() {
            "image" if IMAGE_USE_STATUSES.contains(&status) => {
                debug!(status, id = %event.id, name = %event.name, "image event");
                self.reconcile().await;
                let reference = if event.id.is_empty() {
                    &event.name
                } else {
                    &event.id
                };
                self.touch_reference(reference, &event.name, now);
                self.state.last_event = Some(now);
                self.enforce().await;
                self.save();
            }
            "image" if status == "remove" => {
                debug!(id = %event.id, name = %event.name, "image removed");
                self.state.forget(&event.id);
                self.state.last_event = Some(now);
                self.reconcile().await;
                self.save();
            }
            "container" if CONTAINER_USE_STATUSES.contains(&status) => {
                debug!(status, container = %event.name, image = %event.image, "container event");
                self.reconcile().await;
                self.touch_reference(&event.image, "", now);
                self.state.last_event = Some(now);
                self.save();
            }
            _ => {
                self.state.last_event = Some(now);
            }
        }
    }

    fn touch_reference(&mut self, primary: &str, fallback: &str, now: u64) {
        let image =
            lru::resolve(&self.images, primary).or_else(|| lru::resolve(&self.images, fallback));
        match image {
            Some(image) => {
                let label = image.label();
                let id = image.id.clone();
                let touched = lru::mark_used(&mut self.state, &self.images, &id, now);
                info!(image = %label, chain = touched, "used");
            }
            None => {
                debug!(
                    reference = primary,
                    fallback, "event image not in image list, ignoring"
                );
            }
        }
    }

    /// Refresh the image list and align the state with it.
    async fn reconcile(&mut self) {
        match self.client.list_images().await {
            Ok(images) => {
                let (added, removed) = lru::reconcile(&mut self.state, &images, state::now());
                if added > 0 || removed > 0 {
                    info!(
                        added,
                        removed,
                        total = images.len(),
                        "reconciled image list"
                    );
                }
                self.images = images;
            }
            Err(err) => error!(err = %format!("{err:#}"), "listing images failed"),
        }
    }

    /// Evict least recently used leaf images until the budget is met.
    async fn enforce(&mut self) {
        let mut excluded: HashSet<String> = HashSet::new();
        let mut simulated_freed: i64 = 0;
        loop {
            let df = match self.client.df().await {
                Ok(df) => df,
                Err(err) => {
                    error!(err = %format!("{err:#}"), "disk usage query failed");
                    return;
                }
            };
            let total = df.total_bytes() - simulated_freed;
            if total <= self.cfg.max_size as i64 {
                debug!(total, budget = self.cfg.max_size, "within budget");
                return;
            }
            let unique_sizes: HashMap<String, i64> = df
                .images
                .iter()
                .map(|i| (bare_id(&i.image_id).to_owned(), i.unique_size))
                .collect();
            let victim = lru::pick_victim(
                &self.state,
                &self.images,
                &unique_sizes,
                self.cfg.keep.as_ref(),
                &excluded,
            );
            let Some(victim) = victim.cloned() else {
                warn!(
                    total,
                    budget = self.cfg.max_size,
                    "over budget but no image is evictable"
                );
                return;
            };
            let id = bare_id(&victim.id).to_owned();
            let freed = df.unique_size_of(&id);
            let last_used = self.state.get(&id).unwrap_or(0);
            if self.cfg.dry_run {
                info!(image = %victim.label(), freed, last_used, total, "dry run: would evict");
                simulated_freed += freed;
                excluded.insert(id);
                continue;
            }
            let force = victim.all_names().len() > 1;
            match self.client.remove_image(&id, force).await {
                Ok(RemoveOutcome::Removed(report)) => {
                    info!(image = %victim.label(), freed, last_used, total, deleted = report.deleted.len(), "evicted");
                    self.state.forget(&id);
                    for deleted in &report.deleted {
                        self.state.forget(deleted);
                    }
                    self.reconcile().await;
                }
                Ok(RemoveOutcome::NotFound) => {
                    info!(image = %victim.label(), "already gone");
                    self.state.forget(&id);
                    self.reconcile().await;
                }
                Ok(RemoveOutcome::InUse(message)) => {
                    warn!(image = %victim.label(), message, "eviction refused, skipping this round");
                    excluded.insert(id);
                    self.reconcile().await;
                }
                Err(err) => {
                    error!(image = %victim.label(), err = %format!("{err:#}"), "eviction failed");
                    return;
                }
            }
        }
    }

    fn save(&self) {
        if let Err(err) = self.state.save(&self.cfg.state) {
            error!(err = %format!("{err:#}"), "saving state failed");
        }
    }
}

async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = match signal(SignalKind::terminate()) {
        Ok(term) => term,
        Err(err) => {
            error!(err = %format!("{err:#}"), "cannot listen for SIGTERM");
            let _ = tokio::signal::ctrl_c().await;
            return;
        }
    };
    tokio::select! {
        _ = term.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
}
