#![windows_subsystem = "windows"]

use std::{error::Error, path::PathBuf, str::FromStr};

use iced::{executor, Application, Command, Element, Settings, Subscription, Theme};
extern crate serde;
extern crate serde_json;

use liana::{config::Config as DaemonConfig, miniscript::bitcoin};

use liana_gui::{
    app::{
        self,
        cache::Cache,
        config::{default_datadir, ConfigError},
        wallet::Wallet,
        App,
    },
    installer::{self, Installer},
    launcher::{self, Launcher},
    loader::{self, Loader},
};

#[derive(Debug, PartialEq)]
enum Arg {
    ConfigPath(PathBuf),
    DatadirPath(PathBuf),
    Network(bitcoin::Network),
}

fn parse_args(args: Vec<String>) -> Result<Vec<Arg>, Box<dyn Error>> {
    let mut res = Vec::new();
    for (i, arg) in args.iter().enumerate() {
        if arg == "--conf" {
            if let Some(a) = args.get(i + 1) {
                res.push(Arg::ConfigPath(PathBuf::from(a)));
            } else {
                return Err("missing arg to --conf".into());
            }
        } else if arg == "--datadir" {
            if let Some(a) = args.get(i + 1) {
                res.push(Arg::DatadirPath(PathBuf::from(a)));
            } else {
                return Err("missing arg to --datadir".into());
            }
        } else if arg.contains("--") {
            let network = bitcoin::Network::from_str(args[i].trim_start_matches("--"))?;
            res.push(Arg::Network(network));
        }
    }

    Ok(res)
}

fn log_level_from_config(config: &app::Config) -> Result<log::LevelFilter, Box<dyn Error>> {
    if let Some(level) = &config.log_level {
        match level.as_ref() {
            "info" => Ok(log::LevelFilter::Info),
            "debug" => Ok(log::LevelFilter::Debug),
            "trace" => Ok(log::LevelFilter::Trace),
            _ => Err(format!("Unknown loglevel '{:?}'.", level).into()),
        }
    } else if let Some(true) = config.debug {
        Ok(log::LevelFilter::Debug)
    } else {
        Ok(log::LevelFilter::Info)
    }
}

pub struct GUI {
    theme: Theme,
    state: State,
}

enum State {
    Launcher(Box<Launcher>),
    Installer(Box<Installer>),
    Loader(Box<Loader>),
    App(App),
}

#[derive(Debug)]
pub enum Message {
    CtrlC,
    Launch(Box<launcher::Message>),
    Install(Box<installer::Message>),
    Load(Box<loader::Message>),
    Run(Box<app::Message>),
    Event(iced_native::Event),
}

async fn ctrl_c() -> Result<(), ()> {
    if let Err(e) = tokio::signal::ctrl_c().await {
        log::error!("{}", e);
    };
    log::info!("Signal received, exiting");
    Ok(())
}

impl Application for GUI {
    type Executor = executor::Default;
    type Message = Message;
    type Flags = Config;
    type Theme = iced::Theme;

    fn title(&self) -> String {
        match self.state {
            State::Installer(_) => String::from("Liana Installer"),
            _ => String::from("Liana"),
        }
    }

    fn new(config: Config) -> (GUI, Command<Self::Message>) {
        match config {
            Config::Launcher(datadir_path) => {
                let launcher = Launcher::new(datadir_path);
                (
                    Self {
                        theme: Theme::Light,
                        state: State::Launcher(Box::new(launcher)),
                    },
                    Command::perform(ctrl_c(), |_| Message::CtrlC),
                )
            }
            Config::Install(datadir_path, network) => {
                let (install, command) = Installer::new(datadir_path, network);
                (
                    Self {
                        theme: Theme::Light,
                        state: State::Installer(Box::new(install)),
                    },
                    Command::batch(vec![
                        command.map(|msg| Message::Install(Box::new(msg))),
                        Command::perform(ctrl_c(), |_| Message::CtrlC),
                    ]),
                )
            }
            Config::Run(datadir_path, cfg) => {
                let daemon_cfg =
                    DaemonConfig::from_file(Some(cfg.daemon_config_path.clone())).unwrap();
                let (loader, command) = Loader::new(datadir_path, cfg, daemon_cfg);
                (
                    Self {
                        theme: Theme::Light,
                        state: State::Loader(Box::new(loader)),
                    },
                    Command::batch(vec![
                        command.map(|msg| Message::Load(Box::new(msg))),
                        Command::perform(ctrl_c(), |_| Message::CtrlC),
                    ]),
                )
            }
        }
    }

    fn update(&mut self, message: Self::Message) -> Command<Self::Message> {
        match (&mut self.state, message) {
            (_, Message::CtrlC)
            | (
                _,
                Message::Event(iced_native::Event::Window(
                    iced_native::window::Event::CloseRequested,
                )),
            ) => {
                match &mut self.state {
                    State::Loader(s) => s.stop(),
                    State::Launcher(s) => s.stop(),
                    State::Installer(s) => s.stop(),
                    State::App(s) => s.stop(),
                };
                iced::window::close()
            }
            (State::Launcher(l), Message::Launch(msg)) => match *msg {
                launcher::Message::Install => {
                    let (install, command) =
                        Installer::new(l.datadir_path.clone(), bitcoin::Network::Bitcoin);
                    self.state = State::Installer(Box::new(install));
                    command.map(|msg| Message::Install(Box::new(msg)))
                }
                launcher::Message::Run(network) => {
                    let mut path = l.datadir_path.clone();
                    path.push(network.to_string());
                    path.push(app::config::DEFAULT_FILE_NAME);
                    let cfg = app::Config::from_file(&path).unwrap();
                    let daemon_cfg =
                        DaemonConfig::from_file(Some(cfg.daemon_config_path.clone())).unwrap();
                    let (loader, command) =
                        Loader::new(Some(l.datadir_path.clone()), cfg, daemon_cfg);
                    self.state = State::Loader(Box::new(loader));
                    command.map(|msg| Message::Load(Box::new(msg)))
                }
            },
            (State::Installer(i), Message::Install(msg)) => {
                if let installer::Message::Exit(path) = *msg {
                    let cfg = app::Config::from_file(&path).unwrap();
                    let daemon_cfg =
                        DaemonConfig::from_file(Some(cfg.daemon_config_path.clone())).unwrap();
                    let (loader, command) = Loader::new(None, cfg, daemon_cfg);
                    self.state = State::Loader(Box::new(loader));
                    command.map(|msg| Message::Load(Box::new(msg)))
                } else {
                    i.update(*msg).map(|msg| Message::Install(Box::new(msg)))
                }
            }
            (State::Loader(loader), Message::Load(msg)) => match *msg {
                loader::Message::View(loader::ViewMessage::SwitchNetwork) => {
                    self.state = State::Launcher(Box::new(Launcher::new(
                        loader.datadir_path.clone().unwrap(),
                    )));
                    Command::none()
                }
                loader::Message::Synced(info, coins, spend_txs, daemon) => {
                    let cache = Cache {
                        network: info.network,
                        blockheight: info.block_height,
                        coins,
                        spend_txs,
                        ..Default::default()
                    };

                    let wallet = Wallet::new(info.descriptors.main);

                    let (app, command) = App::new(cache, wallet, loader.gui_config.clone(), daemon);
                    self.state = State::App(app);
                    command.map(|msg| Message::Run(Box::new(msg)))
                }
                _ => loader.update(*msg).map(|msg| Message::Load(Box::new(msg))),
            },
            (State::App(i), Message::Run(msg)) => {
                i.update(*msg).map(|msg| Message::Run(Box::new(msg)))
            }
            _ => Command::none(),
        }
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        Subscription::batch(vec![
            match &self.state {
                State::Installer(v) => v.subscription().map(|msg| Message::Install(Box::new(msg))),
                State::Loader(v) => v.subscription().map(|msg| Message::Load(Box::new(msg))),
                State::App(v) => v.subscription().map(|msg| Message::Run(Box::new(msg))),
                State::Launcher(v) => v.subscription().map(|msg| Message::Launch(Box::new(msg))),
            },
            iced_native::subscription::events().map(Self::Message::Event),
        ])
    }

    fn view(&self) -> Element<Self::Message> {
        match &self.state {
            State::Installer(v) => v.view().map(|msg| Message::Install(Box::new(msg))),
            State::App(v) => v.view().map(|msg| Message::Run(Box::new(msg))),
            State::Launcher(v) => v.view().map(|msg| Message::Launch(Box::new(msg))),
            State::Loader(v) => v.view().map(|msg| Message::Load(Box::new(msg))),
        }
    }

    fn theme(&self) -> Theme {
        self.theme.clone()
    }

    fn scale_factor(&self) -> f64 {
        1.0
    }
}

pub enum Config {
    /// Datadir is optional because app can run with the config path only.
    Run(Option<PathBuf>, app::Config),
    Launcher(PathBuf),
    Install(PathBuf, bitcoin::Network),
}

impl Config {
    pub fn new(
        datadir_path: PathBuf,
        network: Option<bitcoin::Network>,
    ) -> Result<Self, Box<dyn Error>> {
        if let Some(network) = network {
            let mut path = datadir_path.clone();
            path.push(network.to_string());
            path.push(app::config::DEFAULT_FILE_NAME);
            match app::Config::from_file(&path) {
                Ok(cfg) => Ok(Config::Run(Some(datadir_path), cfg)),
                Err(ConfigError::NotFound) => Ok(Config::Install(datadir_path, network)),
                Err(e) => Err(format!("Failed to read configuration file: {}", e).into()),
            }
        } else if !datadir_path.exists() {
            Ok(Config::Install(datadir_path, bitcoin::Network::Bitcoin))
        } else {
            Ok(Config::Launcher(datadir_path))
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = parse_args(std::env::args().collect())?;
    let config = match args.as_slice() {
        [] => {
            let datadir_path = default_datadir().unwrap();
            Config::new(datadir_path, None)
        }
        [Arg::Network(network)] => {
            let datadir_path = default_datadir().unwrap();
            Config::new(datadir_path, Some(*network))
        }
        [Arg::ConfigPath(path)] => Ok(Config::Run(None, app::Config::from_file(path)?)),
        [Arg::DatadirPath(datadir_path)] => Config::new(datadir_path.clone(), None),
        [Arg::DatadirPath(datadir_path), Arg::Network(network)]
        | [Arg::Network(network), Arg::DatadirPath(datadir_path)] => {
            Config::new(datadir_path.clone(), Some(*network))
        }
        _ => {
            return Err("Unknown args combination".into());
        }
    }?;

    let level = if let Config::Run(_, cfg) = &config {
        log_level_from_config(cfg)?
    } else {
        log::LevelFilter::Info
    };
    setup_logger(level)?;

    let mut settings = Settings::with_flags(config);
    settings.exit_on_close_request = false;

    if let Err(e) = GUI::run(settings) {
        return Err(format!("Failed to launch UI: {}", e).into());
    };
    Ok(())
}

// This creates the log file automagically if it doesn't exist, and logs on stdout
// if None is given
pub fn setup_logger(log_level: log::LevelFilter) -> Result<(), fern::InitError> {
    let dispatcher = fern::Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!(
                "[{}][{}][{}] {}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_else(|e| {
                        println!("Can't get time since epoch: '{}'. Using a dummy value.", e);
                        std::time::Duration::from_secs(0)
                    })
                    .as_secs(),
                record.target(),
                record.level(),
                message
            ))
        })
        .level(log_level)
        .level_for("iced_wgpu", log::LevelFilter::Off)
        .level_for("iced_winit", log::LevelFilter::Off)
        .level_for("wgpu_core", log::LevelFilter::Off)
        .level_for("wgpu_hal", log::LevelFilter::Off)
        .level_for("gfx_backend_vulkan", log::LevelFilter::Off)
        .level_for("iced_glutin", log::LevelFilter::Off)
        .level_for("iced_glow", log::LevelFilter::Off)
        .level_for("glow_glyph", log::LevelFilter::Off)
        .level_for("naga", log::LevelFilter::Off)
        .level_for("mio", log::LevelFilter::Off);

    dispatcher.chain(std::io::stdout()).apply()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_args() {
        assert!(parse_args(vec!["--meth".into()]).is_err());
        assert!(parse_args(vec!["--datadir".into()]).is_err());
        assert!(parse_args(vec!["--conf".into()]).is_err());
        assert_eq!(
            Some(vec![
                Arg::DatadirPath(PathBuf::from(".")),
                Arg::ConfigPath(PathBuf::from("hello.toml")),
            ]),
            parse_args(
                "--datadir . --conf hello.toml"
                    .split(' ')
                    .map(|a| a.to_string())
                    .collect()
            )
            .ok()
        );
        assert_eq!(
            Some(vec![Arg::Network(bitcoin::Network::Regtest)]),
            parse_args(vec!["--regtest".into()]).ok()
        );
        assert_eq!(
            Some(vec![
                Arg::DatadirPath(PathBuf::from("hello")),
                Arg::Network(bitcoin::Network::Testnet)
            ]),
            parse_args(
                "--datadir hello --testnet"
                    .split(' ')
                    .map(|a| a.to_string())
                    .collect()
            )
            .ok()
        );
        assert_eq!(
            Some(vec![
                Arg::Network(bitcoin::Network::Testnet),
                Arg::DatadirPath(PathBuf::from("hello"))
            ]),
            parse_args(
                "--testnet --datadir hello"
                    .split(' ')
                    .map(|a| a.to_string())
                    .collect()
            )
            .ok()
        );
    }
}
