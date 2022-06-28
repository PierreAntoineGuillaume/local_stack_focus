#![deny(clippy::all)]
#![warn(clippy::pedantic)]
#![warn(clippy::nursery)]
#![allow(clippy::future_not_send)]

mod business;

use crate::business::{event_loop, Config, RawContainer};
use async_trait::async_trait;
use bollard::container::ListContainersOptions;
use bollard::models::ContainerSummary;
use bollard::Docker;
use business::StringVec;
use std::collections::HashMap;
use std::fs;
use std::io::stdout;

impl From<ContainerSummary> for RawContainer {
    fn from(summary: ContainerSummary) -> Self {
        let networks = summary.network_settings.map_or_else(Vec::new, |settings| {
            settings.networks.map_or_else(Vec::new, |opts| {
                opts.keys().cloned().collect::<Vec<String>>()
            })
        });

        let mut labels = vec![];
        if let Some(map) = summary.labels {
            labels.reserve(map.len());
            for (key, value) in map {
                labels.push(format!("{}={}", key, value));
            }
        }

        let name = summary
            .names
            .unwrap_or_default()
            .get(0)
            .and_then(|name| name.strip_prefix('/').map(ToString::to_string));

        Self {
            id: summary.id.expect("containers must have an id"),
            name,
            networks: StringVec::new(networks),
            labels: StringVec::new(labels),
        }
    }
}

struct DockerImpl {}

#[async_trait]
impl business::Docker for DockerImpl {
    async fn poll(&mut self) -> business::Result<HashMap<String, RawContainer>> {
        let docker = Docker::connect_with_unix_defaults()?;

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
}

fn config() -> business::Result<Config> {
    let config_file = std::env::var("LOCAL_STACK_FOCUS")
        .unwrap_or_else(|_| String::from("/local_stack_focus.toml"));

    let config = fs::read_to_string(config_file)?;
    let config = toml::from_str::<Config>(&config)?;
    Ok(config)
}

#[tokio::main]
async fn main() {
    let config = match config() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("{} error: {}", env!("CARGO_PKG_NAME"), e);
            std::process::exit(1);
        }
    };

    if let Err(e) = event_loop(DockerImpl {}, stdout(), config).await {
        eprintln!("{} error: {}", env!("CARGO_PKG_NAME"), e);
        std::process::exit(1);
    }
}
