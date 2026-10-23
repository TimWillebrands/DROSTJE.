fn main() {
    if let Err(error) = run() {
        eprintln!("drostje-host: {error:#}");
        std::process::exit(1);
    }
}
fn run() -> anyhow::Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    let path = arguments
        .next()
        .ok_or_else(|| anyhow::anyhow!("Usage: drostje-host /absolute/path/host.json"))?;
    anyhow::ensure!(
        arguments.next().is_none(),
        "Usage: drostje-host /absolute/path/host.json"
    );
    let config = drostje::remote::host::HostConfig::load(std::path::Path::new(&path))?;
    drostje::remote::host::run(config)
}
