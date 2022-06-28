use async_trait::async_trait;
use serde::Deserialize;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::io::Write;
use std::time::{Duration, Instant};

type Error = Box<dyn std::error::Error>;
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Deserialize)]
pub struct Config {
    pub(crate) network: String,
    pub(crate) label: String,
    pub(crate) target: String,
    pub(crate) dependencies: Vec<String>,
}

#[async_trait]
pub trait Docker {
    async fn poll(&mut self) -> Result<HashMap<String, RawContainer>>;
}

#[derive(Clone, Debug)]
pub struct StringVec {
    inner: Vec<String>,
}

impl StringVec {
    pub fn new(strings: Vec<String>) -> Self {
        Self { inner: strings }
    }
    pub fn contains(&self, string: &String) -> bool {
        self.inner.contains(string)
    }
}

impl Display for StringVec {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "[")?;
        write!(f, "{}", self.inner.join(", "))?;
        write!(f, "]")
    }
}

#[derive(Clone, Debug)]
pub struct RawContainer {
    pub id: String,
    pub name: Option<String>,
    pub networks: StringVec,
    pub labels: StringVec,
}

impl RawContainer {
    pub fn hash(&self) -> &str {
        &self.id[0..16]
    }

    pub fn name(&self) -> String {
        self.name.clone().unwrap_or_default()
    }
}

#[derive(Clone)]
struct Container {
    id: String,
    name: Option<String>,
}

impl Container {
    pub fn hash(&self) -> &str {
        &self.id[0..16]
    }

    pub fn name(&self) -> String {
        self.name.clone().unwrap_or_default()
    }
}

enum StackEvents {
    New(Container),
    Gone(Container),
    NoFlag(RawContainer),
    OutsideNetwork(RawContainer),
}

struct CurrentStack {
    monitor_network: String,
    monitor_label: String,
    map: Option<HashMap<String, Container>>,
}

impl CurrentStack {
    async fn loop_once<D: Docker, W: Write>(
        &mut self,
        docker: &mut D,
        write: &mut W,
    ) -> Result<()> {
        let containers = docker.poll().await?;
        let events = self.actualize(containers);

        for event in events {
            match event {
                StackEvents::New(container) => {
                    writeln!(
                        write,
                        "event container match: {} {}",
                        container.hash(),
                        container.name(),
                    )?;
                }
                StackEvents::Gone(container) => {
                    writeln!(
                        write,
                        "event container gone: {} {}",
                        container.hash(),
                        container.name()
                    )?;
                }
                StackEvents::NoFlag(container) => {
                    writeln!(
                        write,
                        "event container ignored (label): {} {}",
                        container.hash(),
                        container.name()
                    )?;
                }
                StackEvents::OutsideNetwork(container) => {
                    writeln!(
                        write,
                        "event container ignored (network): {} {}",
                        container.hash(),
                        container.name()
                    )?;
                }
            }
        }

        Ok(())
    }
}

impl CurrentStack {
    fn new(network: String, label: String) -> Self {
        Self {
            monitor_network: network,
            monitor_label: label,
            map: Some(HashMap::default()),
        }
    }
}

impl CurrentStack {
    fn actualize(&mut self, mut raw_containers: HashMap<String, RawContainer>) -> Vec<StackEvents> {
        let mut events = vec![];
        events.reserve(raw_containers.len());

        let known_containers = self.map.take().expect("start");
        let mut new_containers = HashMap::default();

        for (id, container) in known_containers {
            if raw_containers.contains_key(&id) {
                raw_containers.remove(&id);
                new_containers.insert(id.clone(), container);
            } else {
                events.push(StackEvents::Gone(container));
            }
        }

        for (id, new) in raw_containers {
            let inside_network = new.networks.contains(&self.monitor_network);
            let has_flag = new.labels.contains(&self.monitor_label);
            let container = Container {
                id: id.clone(),
                name: new.name.clone(),
            };
            new_containers.insert(id.clone(), container.clone());
            if inside_network && has_flag {
                events.push(StackEvents::New(container));
            } else if inside_network {
                events.push(StackEvents::NoFlag(new));
            } else {
                events.push(StackEvents::OutsideNetwork(new));
            }
        }

        self.map = Some(new_containers);

        events
    }
}

pub async fn event_loop<D: Docker, W: Write>(
    mut docker: D,
    mut write: W,
    config: Config,
) -> Result<()> {
    let tick_rate = Duration::from_secs(1);
    let mut last_tick = Instant::now();
    let mut stack = CurrentStack::new(config.network, config.label);
    writeln!(
        write,
        "Looking for containers in network {} with label {}",
        stack.monitor_network, stack.monitor_label
    )?;
    loop {
        stack.loop_once(&mut docker, &mut write).await?;

        if tick_rate < last_tick.elapsed() {
            std::thread::sleep(
                tick_rate
                    .checked_sub(last_tick.elapsed())
                    .unwrap_or(Duration::from_millis(0)),
            );
            last_tick = Instant::now();
        }
    }
}
