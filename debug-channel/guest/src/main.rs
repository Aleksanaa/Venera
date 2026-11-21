mod ancestry;
mod user_events;
mod engine_events;
#[cfg(any(windows, test))]
mod collector;
#[cfg(any(windows, test))]
mod connection;
#[cfg(any(windows, test))]
mod history;
mod journal;
#[cfg(any(windows, test))]
mod capture_storage;
mod kernel_events;
#[cfg(any(windows, test))]
mod state;
#[cfg(all(test, not(windows)))]
#[path = "windows/system_info_pending.rs"]
mod system_info_pending;
#[cfg(windows)]
mod windows;

fn main() {
    #[cfg(windows)]
    let result = windows::run(std::env::args().skip(1).collect());
    #[cfg(not(windows))]
    let result: Result<(), Box<dyn std::error::Error>> =
        Err("Windows is required to run the guest collector".into());
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
