use rpi_web_extension::{
    config::{self, Launch},
    run_with_host_area,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (launch, area) = config::from_args_with_host_area(std::env::args().skip(1))?;
    match launch {
        Launch::Run(config) => run_with_host_area(*config, area).await?,
        Launch::Help => println!("{}", config::HELP),
        Launch::Version => println!("rpi-web {}", env!("CARGO_PKG_VERSION")),
        Launch::Initialized(path) => println!("Created {}", path.display()),
    }
    Ok(())
}
