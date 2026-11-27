mod appearance;
mod appimage;
mod color_picker;
mod config;
mod desktop;
mod font_picker;
mod icons;
mod library;
mod transaction;
mod ui;
mod widgets;

use std::{ffi::OsString, path::PathBuf, process::ExitCode};

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use crate::library::{Draft, Kind, Store};

const APP_NAME: &str = "Twck";

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Open the GUI and review one or more AppImages or x86-64 executables for import.
    #[arg(long, value_name = "FILE", num_args = 0..)]
    open: Vec<PathBuf>,

    #[command(subcommand)]
    command: Option<Action>,
}

#[derive(Subcommand)]
enum Action {
    /// Register the app and AppImage Open With handler for the current user.
    InstallDesktop,

    /// Add an AppImage or x86-64 executable.
    Add(AddArgs),

    /// Import Gear Lever entries by desktop ID and take over their existing launchers.
    AdoptGearLever {
        #[arg(required = true, value_name = "DESKTOP_ID")]
        desktop_ids: Vec<String>,
    },

    /// List managed apps and their launch IDs.
    List,

    /// Launch an app from its ID and forward any extra arguments.
    Launch {
        id: String,
        #[arg(last = true, value_name = "ARG")]
        arguments: Vec<OsString>,
    },

    /// Remove an entry and its generated launchers.
    Remove {
        id: String,
        /// Also delete an AppImage previously moved by the manager.
        #[arg(long)]
        delete_appimage: bool,
    },
}

#[derive(Args)]
struct AddArgs {
    #[arg(value_enum)]
    kind: Kind,
    /// Path to the AppImage or Linux executable.
    source: String,
    #[arg(long)]
    name: Option<String>,
    #[arg(long)]
    description: Option<String>,
    #[arg(long)]
    icon: Option<PathBuf>,
    #[arg(long)]
    terminal_command: Option<String>,
    #[arg(long = "move", conflicts_with = "keep_in_place")]
    move_appimage: bool,
    #[arg(long)]
    keep_in_place: bool,
    #[arg(long)]
    working_directory: Option<PathBuf>,
    #[arg(last = true, value_name = "ARG")]
    arguments: Vec<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{APP_NAME}: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    let store = Store::open()?;
    match cli.command {
        None => ui::run(store, cli.open)?,
        Some(Action::InstallDesktop) => println!("{}", store.install_desktop()?.display()),
        Some(Action::Add(args)) => {
            anyhow::ensure!(
                !args.move_appimage || args.kind == Kind::AppImage,
                "--move is only available for AppImages"
            );
            let mut draft = Draft::new(args.kind, args.source);
            draft.name = args.name.unwrap_or_default();
            draft.icon = args.icon;
            draft.terminal_command = args.terminal_command;
            draft.working_directory = args.working_directory;
            draft.arguments = args.arguments;
            draft.move_appimage = args.move_appimage
                || (args.kind == Kind::AppImage
                    && !args.keep_in_place
                    && store.settings()?.image.move_to_directory);
            draft.description = args.description.unwrap_or_default();
            println!("Added {}", store.save(draft)?.id);
        }
        Some(Action::AdoptGearLever { desktop_ids }) => {
            for entry in store.adopt_gear_lever(&desktop_ids)? {
                println!("Imported {}", entry.name);
            }
        }
        Some(Action::List) => print_entries(&store)?,
        Some(Action::Launch { id, arguments }) => store.launch(&id, &arguments, true)?,
        Some(Action::Remove {
            id,
            delete_appimage,
        }) => println!("Removed {}", store.remove(&id, delete_appimage)?),
    }
    Ok(())
}

fn print_entries(store: &Store) -> Result<()> {
    for app in store.entries()? {
        println!(
            "{}\t{}\t{}\t{}",
            app.id,
            app.target.kind().label(),
            app.name,
            app.target.display()
        );
    }
    Ok(())
}
