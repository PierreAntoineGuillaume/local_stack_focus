use async_trait::async_trait;
use serde::Deserialize;
use std::collections::HashMap;
use std::fmt::{Debug, Display, Formatter};
use std::io::Write;
use std::time::{Duration, Instant};

type Error = Box<dyn std::error::Error>;
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum DockerError {
    NoName(String),
    NoHost(String),
}

impl Display for DockerError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            DockerError::NoName(id) => write!(f, "container {} has no name, cannot fetch from it", id),
            DockerError::NoHost(id) => write!(f, "container {} has no /etc/hosts file", id),
        }
    }
}

impl std::error::Error for DockerError {}

#[derive(Deserialize)]
pub struct Config {
    pub(crate) network: String,
    pub(crate) label_key: String,
    pub(crate) target: String,
    pub(crate) dependencies: Vec<String>,
}

#[async_trait]
pub trait Docker {
    async fn poll(&mut self) -> Result<HashMap<String, RawContainer>>;
    async fn update_hosts_for(&self, container: Container, dependencies: &[String], network: &str, target: &str, host: &str) -> Result<()>;
}

#[derive(Clone, Debug)]
pub struct StringVec {
    inner: Vec<String>,
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
    pub networks: HashMap<String, String>,
    pub labels: HashMap<String, String>,
}

#[derive(Clone)]
pub struct Container {
    id: String,
    name: Option<String>,
    service: Option<String>,
    ip: Option<String>,
    flag: Option<String>,
}

impl Display for Container {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "container {}", self.hash())?;

        if self.flag.is_some() {
            write!(f, " flagged")?;
        } else {
            write!(f, " unflagged")?;
        }

        if let Some(service) = &self.service {
            write!(f, " service {}", service)?;
        }

        if let Some(name) = &self.name {
            write!(f, " named {}", name)?;
        } else {
            write!(f, " unnamed")?;
        }

        if let Some(ip) = &self.ip {
            write!(f, " in network at ip {}", ip)?;
        } else {
            write!(f, " orphan")?;
        }

        Ok(())
    }
}

impl Container {
    pub fn id(&self) -> String {
        self.id.clone()
    }

    pub fn name(&self) -> Option<String> {
        self.name.clone()
    }

    pub fn hash(&self) -> &str {
        &self.id[0..16]
    }
}

enum StackEvents {
    New(Container),
    Target(Container, Vec<Container>, String),
    Gone(Container),
    NoFlag(Container),
    OutsideNetwork(Container),
}

struct CurrentStack {
    config: Config,
    target_ip: Option<String>,
    map: Option<HashMap<String, Container>>,
}

impl CurrentStack {
    async fn loop_once<D: Docker, W: Write>(&mut self, docker: &mut D, f: &mut W) -> Result<()> {
        let containers = docker.poll().await?;
        let events = self.actualize(containers);

        for event in events {
            match event {
                StackEvents::Target(container, known, ip) => {
                    writeln!(f, "event found target: {} applying it to known {} containers", container, known.len())?;
                    for item in known {
                        writeln!(f, "updating previous container {}", item.hash())?;
                        docker.update_hosts_for(item, &self.config.dependencies, &self.config.network, &self.config.network, &ip).await?;
                    }
                    writeln!(f, "recording ip for target: {}", ip)?;
                    self.target_ip = Some(ip);
                }
                StackEvents::New(container) => {
                    writeln!(f, "event container match: {}", container)?;
                    if let Some(ip) = &self.target_ip {
                        writeln!(f, "updating /etc/hosts for container {}", container.hash())?;
                        docker.update_hosts_for(container, &self.config.dependencies, &self.config.network, &self.config.target, ip).await?;
                    } else {
                        writeln!(f, "could not update /etc/hosts for container {} because no target known yet", container.hash())?;
                    }
                }
                StackEvents::Gone(container) => {
                    writeln!(f, "event container gone: {}", container)?;
                }
                StackEvents::NoFlag(container) => {
                    writeln!(f, "event container ignored (label): {}", container, )?;
                }
                StackEvents::OutsideNetwork(container) => {
                    writeln!(f, "event container ignored (network): {}", container, )?;
                }
            }
        }

        Ok(())
    }
}

impl CurrentStack {
    fn new(config: Config) -> Self {
        Self {
            config,
            map: Some(HashMap::default()),
            target_ip: None,
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
            let ip = new.networks.get(&self.config.network);
            let service = new.labels.get("com.docker.compose.service").cloned();

            let flag = new.labels.get(&self.config.label_key);

            let c = Container {
                id: id.clone(),
                name: new.name.clone(),
                service: service.clone(),
                ip: ip.cloned(),
                flag: flag.cloned(),
            };

            let container = c.clone();

            if ip.is_some() && Some(&self.config.target) == service.as_ref() {
                events.push(StackEvents::Target(container, new_containers.values().filter(|item| {
                    item.flag.is_some() && item.ip.is_some()
                }).cloned().collect(), ip.cloned().unwrap()));
            } else if ip.is_some() && flag.is_some() {
                events.push(StackEvents::New(container));
            } else if ip.is_some() {
                events.push(StackEvents::NoFlag(container));
            } else {
                events.push(StackEvents::OutsideNetwork(container));
            }

            new_containers.insert(id.clone(), c);
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
    let mut stack = CurrentStack::new(config);
    writeln!(
        write,
        "Looking for containers in network {} with label {} to be routed via service «{}»",
        stack.config.network, stack.config.label_key, stack.config.target
    )?;
    loop {
        stack.loop_once(&mut docker, &mut write).await?;

        if tick_rate > last_tick.elapsed() {
            std::thread::sleep(
                tick_rate
                    .checked_sub(last_tick.elapsed())
                    .unwrap_or(Duration::from_millis(0)),
            );
            last_tick = Instant::now();
        }
    }
}

pub fn update_host_file(file: String, lines: &[String], network: &str, target: &str, host: &str) -> String {
    const PACKAGE: &str = env!("CARGO_PKG_NAME");

    let open_guard = format!("### open {} {} {}\n", PACKAGE,network, target);
    let close_guard = format!("### close {} {} {}\n", PACKAGE, network, target);

    let content = trim_host_from_guards(file, &open_guard, &close_guard);

    format!(
        "{}{open_guard}{}{close_guard}",
        content,
        lines.iter().map(|str| format!("{}\t{}\n", host, str)).collect::<Vec<String>>().join("")
    )
}

fn trim_host_from_guards(file: String, open_guard: &str, close_guard: &str) -> String {
    let mut content = String::new();

    if let Some(hosts) = file.split(open_guard).next() {
        content.push_str(hosts);
    }
    if let Some(hosts) = file.split(close_guard).nth(1) {
        content.push_str(hosts);
    }
    content
}

#[cfg(test)]
mod tests {
    use crate::business::trim_host_from_guards;

    const PACKAGE: &str = env!("CARGO_PKG_NAME");

    #[test]
    pub fn test_remove() {
        let host_file = format!("
1.1.1.1 toto
### open guard guard
### close guard guard
1.2.3.4 titi
");

        let str = "
1.1.1.1 toto
1.2.3.4 titi
".to_string();
        assert_eq!(
            trim_host_from_guards(host_file, "### open guard guard\n", "### close guard guard\n"),
            str
        )
    }

    #[test]
    pub fn upload_host_file_host_file() {
        let host_file = format!(
            "127.0.0.1	localhost

# The following lines are desirable for IPv6 capable hosts
::1     ip6-localhost ip6-loopback
fe00::0 ip6-localnet
ff00::0 ip6-mcastprefix
ff02::1 ip6-allnodes
ff02::2 ip6-allrouters
::1 traefik.localhost
::1 custom_app.localhost
### open {} network target
### close {} network target
1.1.1.1 aze
", PACKAGE, PACKAGE);
        let lines = vec![
            "web".into(),
            "api".into(),
        ];
        let s = super::update_host_file(host_file, &lines, "network".into(), "target".into(), "1.1.1.1".into());
        assert_eq!(s,
            format!(
"127.0.0.1	localhost

# The following lines are desirable for IPv6 capable hosts
::1     ip6-localhost ip6-loopback
fe00::0 ip6-localnet
ff00::0 ip6-mcastprefix
ff02::1 ip6-allnodes
ff02::2 ip6-allrouters
::1 traefik.localhost
::1 custom_app.localhost
1.1.1.1 aze
### open {} network target
1.1.1.1\tweb
1.1.1.1\tapi
### close {} network target
", PACKAGE, PACKAGE)
        );
    }
}

