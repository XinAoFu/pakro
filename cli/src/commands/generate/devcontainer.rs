use std::collections::HashMap;

use crate::{
    dirs,
    file::{self, display_path},
    git::Git,
};
use serde::Serialize;

/// Generate devcontainer configuration for pakro
///
/// Requires --image pointing to an image containing your local pakro build.
/// Prints JSON by default. `--write` saves .devcontainer/devcontainer.json;
/// review the image, mounts, and generated setup commands before opening it.
#[derive(Debug, usage_rs::Args)]
#[usage(
    verbatim_doc_comment,
    example(r###"pakro generate devcontainer --image pakro-dev:latest"###)
)]
pub(super) struct Devcontainer {
    /// The image to use for the devcontainer
    #[usage(long, short, verbatim_doc_comment)]
    image: Option<String>,

    /// Bind the pakro-data-volume to the devcontainer
    #[usage(long, short, verbatim_doc_comment)]
    mount_pakro_data: bool,

    /// The name of the devcontainer
    #[usage(long, short, verbatim_doc_comment)]
    name: Option<String>,

    /// Write to .devcontainer/devcontainer.json
    #[usage(long, short)]
    write: bool,
}

#[derive(Serialize)]
struct DevcontainerTemplate {
    name: String,
    image: String,
    features: HashMap<String, HashMap<String, String>>,
    customizations: HashMap<String, HashMap<String, Vec<String>>>,
    mounts: Vec<DevcontainerMount>,
    #[serde(rename = "containerEnv")]
    container_env: HashMap<String, String>,
    #[serde(rename = "remoteEnv")]
    remote_env: HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "postCreateCommand")]
    post_create_command: Option<String>,
}

#[derive(Serialize)]
struct DevcontainerMount {
    source: String,
    target: String,
    #[serde(rename = "type")]
    type_field: String,
}

impl Devcontainer {
    pub(super) async fn run(self) -> eyre::Result<()> {
        let output = self.generate()?;

        if self.write {
            let path = match Git::get_root() {
                Ok(root) => root.join(".devcontainer/devcontainer.json"),
                Err(_) => dirs::CWD
                    .as_ref()
                    .unwrap()
                    .join(".devcontainer/devcontainer.json"),
            };
            file::create(&path)?;
            file::write(&path, &output)?;
            pakroprintln!("Wrote to {}", display_path(&path));
        } else {
            pakroprintln!("{output}");
        }

        Ok(())
    }

    fn generate(&self) -> eyre::Result<String> {
        let name = self.name.as_deref().unwrap_or("pakro");
        let image = self
            .image
            .as_deref()
            .ok_or_else(|| eyre::eyre!("pakro devcontainer image is not configured; pass --image with an image containing your pakro build"))?;

        let mut post_create_command: Option<String> = None;
        let mut mounts = vec![];
        let mut container_env = HashMap::new();
        let mut remote_env = HashMap::new();
        if self.mount_pakro_data {
            mounts.push(DevcontainerMount {
                source: "pakro-data-volume".to_string(),
                target: "/mnt/pakro-data".to_string(),
                type_field: "volume".to_string(),
            });
            container_env.insert("PAKRO_DATA_DIR".to_string(), "/mnt/pakro-data".to_string());
            remote_env.insert(
                "PATH".to_string(),
                "${containerEnv:PATH}:/mnt/pakro-data/shims".to_string(),
            );
            post_create_command = Some("sudo chown -R vscode:vscode /mnt/pakro-data".to_string());
        }

        // No first-party feature or editor extension has been published.
        let features = HashMap::new();
        let customizations = HashMap::new();

        let template = DevcontainerTemplate {
            name: name.to_string(),
            image: image.to_string(),
            features,
            customizations,
            mounts,
            container_env,
            remote_env,
            post_create_command,
        };

        let output = serde_json::to_string_pretty(&template)?;

        Ok(output)
    }
}
