#[cfg(any(windows, test))]
mod ancestry;
#[cfg(any(windows, test))]
mod collector;
#[cfg(any(windows, test))]
mod state;
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
