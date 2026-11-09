//! JSON shapes of the libpod REST API (podman 4.9) that the daemon consumes.
//!
//! Only the fields the daemon needs are declared; unknown fields are ignored.
//! Go marshals nil slices as `null`, so every collection goes through
//! [`nullable`], which maps `null` to the type's default.

use serde::{Deserialize, Deserializer};

fn nullable<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// One line of `GET /libpod/events`, already flattened.
///
/// The wire format is Docker's event message (`Type`, `Action`, `Actor.ID`,
/// `Actor.Attributes`), which podman fills from its own event: `Action` is the
/// status, `Actor.ID` the object ID, `Attributes.name` the object name and
/// `Attributes.image` the image of a container event. For a `build` event
/// podman puts the new image ID in `name` and leaves `Actor.ID` empty.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Event {
    pub kind: String,
    pub status: String,
    pub id: String,
    pub name: String,
    pub image: String,
}

#[derive(Debug, Default, Deserialize)]
struct WireEvent {
    #[serde(rename = "Type", default, deserialize_with = "nullable")]
    kind: String,
    #[serde(rename = "Action", default, deserialize_with = "nullable")]
    action: String,
    #[serde(default, deserialize_with = "nullable")]
    status: String,
    #[serde(default, deserialize_with = "nullable")]
    id: String,
    #[serde(default, deserialize_with = "nullable")]
    from: String,
    #[serde(rename = "Actor", default, deserialize_with = "nullable")]
    actor: WireActor,
}

#[derive(Debug, Default, Deserialize)]
struct WireActor {
    #[serde(rename = "ID", default, deserialize_with = "nullable")]
    id: String,
    #[serde(rename = "Attributes", default, deserialize_with = "nullable")]
    attributes: std::collections::HashMap<String, String>,
}

impl<'de> Deserialize<'de> for Event {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut wire = WireEvent::deserialize(deserializer)?;
        let name = wire.actor.attributes.remove("name").unwrap_or_default();
        let image = wire.actor.attributes.remove("image").unwrap_or_default();
        Ok(Event {
            kind: wire.kind,
            status: if wire.action.is_empty() {
                wire.status
            } else {
                wire.action
            },
            id: if wire.actor.id.is_empty() {
                wire.id
            } else {
                wire.actor.id
            },
            name,
            image: if image.is_empty() { wire.from } else { image },
        })
    }
}

/// One entry of `GET /libpod/images/json?all=true`.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct ImageSummary {
    #[serde(rename = "Id")]
    pub id: String,
    #[serde(rename = "ParentId", default, deserialize_with = "nullable")]
    pub parent_id: String,
    #[serde(rename = "RepoTags", default, deserialize_with = "nullable")]
    pub repo_tags: Vec<String>,
    #[serde(rename = "Names", default, deserialize_with = "nullable")]
    pub names: Vec<String>,
    #[serde(rename = "Size", default)]
    pub size: i64,
    #[serde(rename = "Containers", default)]
    pub containers: i64,
    #[serde(rename = "Dangling", default)]
    pub dangling: bool,
    #[serde(rename = "Created", default)]
    pub created: i64,
}

impl ImageSummary {
    /// Every name podman knows this image by (tags and `Names` overlap, so dedup).
    pub fn all_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .names
            .iter()
            .chain(self.repo_tags.iter())
            .map(String::as_str)
            .filter(|n| !n.is_empty() && *n != "<none>:<none>")
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    /// Short identifier for logs: first name, else 12 hex chars of the ID.
    pub fn label(&self) -> String {
        match self.all_names().first() {
            Some(name) => format!("{} ({})", name, short_id(&self.id)),
            None => short_id(&self.id),
        }
    }
}

/// `GET /libpod/system/df`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DfReport {
    #[serde(rename = "ImagesSize", default)]
    pub images_size: i64,
    #[serde(rename = "Images", default, deserialize_with = "nullable")]
    pub images: Vec<DfImage>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DfImage {
    #[serde(rename = "ImageID", default, deserialize_with = "nullable")]
    pub image_id: String,
    #[serde(rename = "UniqueSize", default)]
    pub unique_size: i64,
}

impl DfReport {
    /// Bytes used by all images, shared layers counted once.
    ///
    /// `ImagesSize` is what `podman system df` prints as the images total. Older
    /// servers leave it at zero; then the sum of unique sizes is the best lower bound.
    pub fn total_bytes(&self) -> i64 {
        if self.images_size > 0 {
            return self.images_size;
        }
        self.images.iter().map(|i| i.unique_size).sum()
    }

    /// Bytes freed by deleting the given image alone.
    pub fn unique_size_of(&self, id: &str) -> i64 {
        self.images
            .iter()
            .find(|i| same_id(&i.image_id, id))
            .map(|i| i.unique_size)
            .unwrap_or(0)
    }
}

/// `DELETE /libpod/images/{name}` body.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct RemoveReport {
    #[serde(rename = "Deleted", default, deserialize_with = "nullable")]
    pub deleted: Vec<String>,
    #[serde(rename = "Untagged", default, deserialize_with = "nullable")]
    pub untagged: Vec<String>,
    #[serde(rename = "ExitCode", default)]
    pub exit_code: i64,
    #[serde(rename = "Errors", default, deserialize_with = "nullable")]
    pub errors: Vec<String>,
}

/// Strip an optional `sha256:` prefix so IDs from different endpoints compare equal.
pub fn bare_id(id: &str) -> &str {
    id.strip_prefix("sha256:").unwrap_or(id)
}

pub fn same_id(a: &str, b: &str) -> bool {
    bare_id(a) == bare_id(b)
}

pub fn short_id(id: &str) -> String {
    bare_id(id).chars().take(12).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_collections_become_empty() {
        let json = r#"{"Id":"abc","ParentId":"","RepoTags":null,"Names":null,"Size":3}"#;
        let img: ImageSummary = serde_json::from_str(json).unwrap();
        assert!(img.repo_tags.is_empty());
        assert!(img.names.is_empty());
        assert_eq!(img.size, 3);
    }

    #[test]
    fn event_parses_wire_shape() {
        // build: podman 4.9 puts the image ID in Attributes.name and leaves Actor.ID empty
        let json = r#"{"status":"build","id":"","Type":"image","Action":"build","Actor":{"ID":"","Attributes":{"name":"b0c12dbf921d","podId":""}},"scope":"local","time":1762684340,"timeNano":1762684340126995254}"#;
        let ev: Event = serde_json::from_str(json).unwrap();
        assert_eq!(ev.kind, "image");
        assert_eq!(ev.status, "build");
        assert_eq!(ev.id, "");
        assert_eq!(ev.name, "b0c12dbf921d");
        // container create
        let json = r#"{"status":"create","id":"c0ffee","from":"localhost/img1:latest","Type":"container","Action":"create","Actor":{"ID":"c0ffee","Attributes":{"image":"localhost/img1:latest","name":"touch1","podId":""}},"scope":"local","time":1,"timeNano":1}"#;
        let ev: Event = serde_json::from_str(json).unwrap();
        assert_eq!(ev.kind, "container");
        assert_eq!(ev.status, "create");
        assert_eq!(ev.id, "c0ffee");
        assert_eq!(ev.image, "localhost/img1:latest");
        assert_eq!(ev.name, "touch1");
        // null attributes and missing actor
        let ev: Event =
            serde_json::from_str(r#"{"Type":"system","Action":"refresh","Actor":null}"#).unwrap();
        assert_eq!(ev.status, "refresh");
    }

    #[test]
    fn df_total_prefers_images_size() {
        let df = DfReport {
            images_size: 100,
            images: vec![DfImage {
                unique_size: 1,
                ..Default::default()
            }],
        };
        assert_eq!(df.total_bytes(), 100);
        let df = DfReport {
            images_size: 0,
            images: vec![
                DfImage {
                    unique_size: 1,
                    ..Default::default()
                },
                DfImage {
                    unique_size: 2,
                    ..Default::default()
                },
            ],
        };
        assert_eq!(df.total_bytes(), 3);
    }

    #[test]
    fn names_dedup_and_drop_none() {
        let img = ImageSummary {
            id: "0123456789abcdef".into(),
            repo_tags: vec!["<none>:<none>".into(), "localhost/a:1".into()],
            names: vec!["localhost/a:1".into()],
            ..Default::default()
        };
        assert_eq!(img.all_names(), vec!["localhost/a:1"]);
        assert_eq!(img.label(), "localhost/a:1 (0123456789ab)");
    }
}
