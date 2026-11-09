//! Pure eviction logic: which images count as used, and which one goes next.

use std::collections::{HashMap, HashSet};

use regex::Regex;
use tracing::debug;

use crate::model::{bare_id, same_id, ImageSummary};
use crate::state::{State, Timestamp};

/// Image event statuses after which the named image (and its parent chain) counts as used.
pub const IMAGE_USE_STATUSES: &[&str] = &[
    "build",
    "pull",
    "commit",
    "import",
    "loadfromarchive",
    "tag",
    "untag",
    "push",
    "save",
    "mount",
];

/// Container event statuses that mean the container's image is in use.
pub const CONTAINER_USE_STATUSES: &[&str] = &["create", "start"];

/// Bring the state in line with what podman has: unknown images are considered
/// used right now (they were most likely just built while we were not looking),
/// vanished images are forgotten. Returns (added, removed).
pub fn reconcile(state: &mut State, images: &[ImageSummary], now: Timestamp) -> (usize, usize) {
    let present: HashSet<&str> = images.iter().map(|i| bare_id(&i.id)).collect();
    let stale: Vec<String> = state
        .last_used
        .keys()
        .filter(|id| !present.contains(id.as_str()))
        .cloned()
        .collect();
    for id in &stale {
        state.forget(id);
    }
    let mut added = 0;
    for id in present {
        if state.get(id).is_none() {
            state.touch(id, now);
            added += 1;
        }
    }
    (added, stale.len())
}

/// Mark an image and every ancestor (via `ParentId`) as used at `now`.
/// Returns the number of images touched.
pub fn mark_used(state: &mut State, images: &[ImageSummary], id: &str, now: Timestamp) -> usize {
    let by_id: HashMap<&str, &ImageSummary> = images.iter().map(|i| (bare_id(&i.id), i)).collect();
    let mut touched = 0;
    let mut seen = HashSet::new();
    let mut current = bare_id(id).to_owned();
    while !current.is_empty() && seen.insert(current.clone()) {
        state.touch(&current, now);
        touched += 1;
        current = match by_id.get(current.as_str()) {
            Some(image) => bare_id(&image.parent_id).to_owned(),
            None => break,
        };
    }
    touched
}

/// Find an image by ID (full, prefixed or 12-char short) or by name.
///
/// Event names are fully qualified (`localhost/foo:latest`, `docker.io/library/alpine:3`),
/// but container events may carry what the user typed, so a trailing path match
/// (`alpine:3` for `docker.io/library/alpine:3`) and a tagless match are accepted.
pub fn resolve<'a>(images: &'a [ImageSummary], reference: &str) -> Option<&'a ImageSummary> {
    let reference = reference.trim();
    if reference.is_empty() {
        return None;
    }
    let wanted = bare_id(reference);
    if let Some(image) = images.iter().find(|i| same_id(&i.id, wanted)) {
        return Some(image);
    }
    if wanted.len() >= 12 && wanted.chars().all(|c| c.is_ascii_hexdigit()) {
        let mut matches = images.iter().filter(|i| bare_id(&i.id).starts_with(wanted));
        if let Some(image) = matches.next() {
            if matches.next().is_none() {
                return Some(image);
            }
        }
    }
    let exact = images.iter().find(|i| i.all_names().contains(&reference));
    if exact.is_some() {
        return exact;
    }
    let tagged = if reference
        .rsplit('/')
        .next()
        .is_some_and(|last| last.contains(':'))
    {
        reference.to_owned()
    } else {
        format!("{reference}:latest")
    };
    images.iter().find(|i| {
        i.all_names().iter().any(|n| {
            *n == tagged
                || n.ends_with(&format!("/{tagged}"))
                || n.ends_with(&format!("/{reference}"))
        })
    })
}

/// Pick the next image to evict, or `None` when nothing may go.
///
/// Candidates have no containers, no child image, no name matching `keep`, and
/// are not in `excluded` (refused earlier this round). Oldest last use first;
/// among equals, the one freeing the most bytes.
pub fn pick_victim<'a>(
    state: &State,
    images: &'a [ImageSummary],
    unique_sizes: &HashMap<String, i64>,
    keep: Option<&Regex>,
    excluded: &HashSet<String>,
) -> Option<&'a ImageSummary> {
    let parents: HashSet<&str> = images
        .iter()
        .map(|i| bare_id(&i.parent_id))
        .filter(|p| !p.is_empty())
        .collect();
    images
        .iter()
        .filter(|image| {
            let id = bare_id(&image.id);
            if image.containers > 0 || parents.contains(id) || excluded.contains(id) {
                return false;
            }
            if let Some(keep) = keep {
                if image.all_names().iter().any(|n| keep.is_match(n)) {
                    debug!(image = %image.label(), "protected by keep pattern");
                    return false;
                }
            }
            true
        })
        .min_by_key(|image| {
            let id = bare_id(&image.id);
            let last_used = state.get(id).unwrap_or(0);
            let freed = unique_sizes.get(id).copied().unwrap_or(image.size);
            (last_used, std::cmp::Reverse(freed))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(id: &str, parent: &str, names: &[&str]) -> ImageSummary {
        ImageSummary {
            id: id.to_string(),
            parent_id: parent.to_string(),
            names: names.iter().map(|s| s.to_string()).collect(),
            repo_tags: names.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    /// base <- mid <- leaf_a ; base <- leaf_b ; lone
    fn fixture() -> Vec<ImageSummary> {
        vec![
            image("ba5e000000000000", "", &["docker.io/library/alpine:3.19"]),
            image("a1d0000000000000", "ba5e000000000000", &[]),
            image("1eafa00000000000", "a1d0000000000000", &["localhost/app:1"]),
            image(
                "1eafb00000000000",
                "ba5e000000000000",
                &["localhost/other:2", "localhost/other:latest"],
            ),
            image("10ae000000000000", "", &["localhost/pandoc:latest"]),
        ]
    }

    #[test]
    fn reconcile_adds_unknown_and_forgets_vanished() {
        let images = fixture();
        let mut state = State::default();
        state.touch("gone", 1);
        let (added, removed) = reconcile(&mut state, &images, 100);
        assert_eq!((added, removed), (5, 1));
        assert_eq!(state.get("10ae000000000000"), Some(100));
        assert_eq!(state.get("gone"), None);
        assert_eq!(reconcile(&mut state, &images, 200), (0, 0));
        assert_eq!(state.get("10ae000000000000"), Some(100));
    }

    #[test]
    fn mark_used_walks_parent_chain() {
        let images = fixture();
        let mut state = State::default();
        reconcile(&mut state, &images, 1);
        assert_eq!(
            mark_used(&mut state, &images, "sha256:1eafa00000000000", 50),
            3
        );
        assert_eq!(state.get("1eafa00000000000"), Some(50));
        assert_eq!(state.get("a1d0000000000000"), Some(50));
        assert_eq!(state.get("ba5e000000000000"), Some(50));
        assert_eq!(state.get("1eafb00000000000"), Some(1));
        assert_eq!(state.get("10ae000000000000"), Some(1));
    }

    #[test]
    fn mark_used_survives_parent_cycles_and_unknown_ids() {
        let images = vec![image("a", "b", &[]), image("b", "a", &[])];
        let mut state = State::default();
        assert_eq!(mark_used(&mut state, &images, "a", 5), 2);
        assert_eq!(mark_used(&mut state, &images, "zzz", 6), 1);
        assert_eq!(state.get("zzz"), Some(6));
    }

    #[test]
    fn resolve_by_id_and_names() {
        let images = fixture();
        assert_eq!(
            resolve(&images, "1eafa00000000000").unwrap().id,
            "1eafa00000000000"
        );
        assert_eq!(
            resolve(&images, "sha256:1eafa00000000000").unwrap().id,
            "1eafa00000000000"
        );
        assert_eq!(
            resolve(&images, "1eafa0000000").unwrap().id,
            "1eafa00000000000"
        );
        assert_eq!(
            resolve(&images, "localhost/app:1").unwrap().id,
            "1eafa00000000000"
        );
        assert_eq!(
            resolve(&images, "alpine:3.19").unwrap().id,
            "ba5e000000000000"
        );
        assert_eq!(
            resolve(&images, "localhost/other").unwrap().id,
            "1eafb00000000000"
        );
        assert!(resolve(&images, "nothing/here:1").is_none());
        assert!(resolve(&images, "").is_none());
    }

    #[test]
    fn victim_is_oldest_leaf_without_containers() {
        let images = fixture();
        let mut state = State::default();
        for (id, t) in [
            ("ba5e000000000000", 10),
            ("a1d0000000000000", 10),
            ("1eafa00000000000", 10),
            ("1eafb00000000000", 20),
            ("10ae000000000000", 5),
        ] {
            state.touch(id, t);
        }
        let sizes = HashMap::new();
        let none = HashSet::new();
        // lone is oldest and a leaf
        let victim = pick_victim(&state, &images, &sizes, None, &none).unwrap();
        assert_eq!(victim.id, "10ae000000000000");
        // protected by keep -> next oldest leaf is leafa (base and mid are parents)
        let keep = Regex::new("pandoc").unwrap();
        let victim = pick_victim(&state, &images, &sizes, Some(&keep), &none).unwrap();
        assert_eq!(victim.id, "1eafa00000000000");
        // refused this round -> leafb
        let excluded: HashSet<String> = ["10ae000000000000", "1eafa00000000000"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let victim = pick_victim(&state, &images, &sizes, None, &excluded).unwrap();
        assert_eq!(victim.id, "1eafb00000000000");
    }

    #[test]
    fn victim_tie_breaks_on_freed_bytes_and_skips_running() {
        let mut images = fixture();
        let mut state = State::default();
        reconcile(&mut state, &images, 1);
        let mut sizes = HashMap::new();
        sizes.insert("1eafa00000000000".to_string(), 10);
        sizes.insert("1eafb00000000000".to_string(), 500);
        sizes.insert("10ae000000000000".to_string(), 100);
        let none = HashSet::new();
        assert_eq!(
            pick_victim(&state, &images, &sizes, None, &none)
                .unwrap()
                .id,
            "1eafb00000000000"
        );
        images[3].containers = 1;
        assert_eq!(
            pick_victim(&state, &images, &sizes, None, &none)
                .unwrap()
                .id,
            "10ae000000000000"
        );
        for image in &mut images {
            image.containers = 1;
        }
        assert!(pick_victim(&state, &images, &sizes, None, &none).is_none());
    }
}
