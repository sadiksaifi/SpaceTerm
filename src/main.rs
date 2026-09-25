mod app;
mod appearance;
mod application_identity;
mod close_confirmation;
mod domain;
mod platform;
mod settings;
mod ssh;
mod terminal;
mod theme;
mod ui;

mod desktop_profile;

#[cfg(feature = "performance-probes")]
mod performance_probes;

fn main() {
    #[cfg(feature = "performance-probes")]
    if performance_probes::start().is_err() {
        eprintln!("could not start performance sampler");
        std::process::exit(1);
    }
    platform::main();
}

#[cfg(test)]
mod architecture_tests;

mod directory_selection;
mod local_path;
