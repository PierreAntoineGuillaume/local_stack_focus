#![deny(clippy::all)]
#![warn(clippy::pedantic)]
#![warn(clippy::nursery)]
#![allow(clippy::future_not_send)]

mod business;
use futures_util::stream::TryStreamExt;

use crate::business::{event_loop, Config, RawContainer, DockerError};
use async_trait::async_trait;
use bollard::container::{DownloadFromContainerOptions, ListContainersOptions};
use bollard::models::ContainerSummary;
use bollard::Docker;
use std::collections::HashMap;
use std::fs;
use std::io::{Read, stdout};
use std::process::Command;
use bollard::exec::{CreateExecOptions, StartExecOptions};

impl From<ContainerSummary> for RawContainer {
    fn from(summary: ContainerSummary) -> Self {
        let networks: HashMap<String, String> =
            summary
                .network_settings
                .map_or_else(HashMap::new, |settings| {
                    settings.networks.map_or_else(HashMap::new, |map| {
                        map.iter()
                            .flat_map(|(key, val)| {
                                if let Some(ip) = &val.ip_address {
                                    Some((key.clone(), ip.clone()))
                                } else {
                                    None
                                }
                            })
                            .collect()
                    })
                });

        let name = summary
            .names
            .unwrap_or_default()
            .get(0)
            .and_then(|name| name.strip_prefix('/').map(ToString::to_string));

        Self {
            id: summary.id.expect("containers must have an id"),
            name,
            networks,
            labels: summary.labels.unwrap_or_default(),
        }
    }
}

struct DockerImpl {
    wrap: Docker
}

impl DockerImpl {
    pub fn new() -> business::Result<Self> {
        Ok(Self {
            wrap: Docker::connect_with_unix_defaults()?
        })
    }
}

#[async_trait]
impl business::Docker for DockerImpl {
    async fn poll(&mut self) -> business::Result<HashMap<String, RawContainer>> {
        let docker = &self.wrap;

        let opts = Some(ListContainersOptions::<&str>::default());
        let list = docker.list_containers(opts).await?;
        Ok(list
            .into_iter()
            .map(|container| {
                let raw = RawContainer::from(container);
                (raw.id.clone(), raw)
            })
            .collect::<HashMap<String, RawContainer>>())
    }

    async fn update_hosts_for(&self, container: business::Container, dependencies: &[String], network: &str, target: &str, host: &str) -> business::Result<()> {
        let name = container.name().ok_or(DockerError::NoName(container.id()))?;
        let opts = Some(DownloadFromContainerOptions{path: "/etc/hosts", ..Default::default()});
        let res = self.wrap.download_from_container(&name, opts);

        let bytes = res.try_fold(Vec::new(), |mut acc, chunk| async move {
            acc.extend_from_slice(&chunk[..]);
            Ok(acc)
        }).await?;

        let mut a: tar::Archive<&[u8]> = tar::Archive::new(&bytes[..]);
        let mut buffer = String::new();
        let _ = a.entries()
            .or_else(|_| Err(DockerError::NoHost(container.id())))?
            .nth(0).ok_or_else(|| DockerError::NoHost(container.id()))??
            .read_to_string(&mut buffer)?
            ;
        let buffer = buffer.replace("\\t", "\t").replace("\\n", "\n").to_string();
        let new_host_file = business::update_host_file(buffer, dependencies, network, target, host);

        Command::new("docker")
            .args(&["exec", "-u", "root", &container.id(), "sh", "-c", &format!(r#"echo "{}" > /etc/hosts"#, new_host_file)])
            .output()?;

        Ok(())
    }
}

fn config() -> business::Result<Config> {
    let config_file = std::env::var("LOCAL_STACK_FOCUS")
        .unwrap_or_else(|_| String::from("/local_stack_focus.toml"));

    let config = fs::read_to_string(config_file)?;
    let config = toml::from_str::<Config>(&config)?;
    Ok(config)
}

async fn wrap() -> business::Result<()> {
    let config = config()?;
    event_loop(DockerImpl::new()?, stdout(), config).await
}

#[tokio::main]
async fn main() {
    if let Err(e) = wrap().await {
        eprintln!("{} error: {}", env!("CARGO_PKG_NAME"), e);
        std::process::exit(1);
    }
}
