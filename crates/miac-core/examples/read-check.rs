//! Read-only device diagnostics. Never sends set_properties or actions.
use miac_core::{controller::Controller, credentials::Credentials, Transport};

fn main() {
    let mut controller = Controller::new(Credentials::appdata(), Transport::Auto);
    match controller.init_transport() {
        Ok(link) => println!("transport: {}", link.label()),
        Err(error) => { eprintln!("initialization failed: {error}"); return; }
    }
    match controller.snapshot() {
        Ok(snapshot) => {
            for (name, value) in snapshot.status {
                if ["on", "targetTemp", "electricity"].contains(&name) {
                    println!("{name}: {}", value.display());
                }
            }
        }
        Err(error) => eprintln!("snapshot failed: {error}"),
    }
    match controller.power_stats() {
        Ok(power) => println!("history: today={} kWh, month={} kWh, year={} kWh", power.today_energy, power.month_energy, power.year_energy),
        Err(error) => eprintln!("history failed: {error}"),
    }
}
