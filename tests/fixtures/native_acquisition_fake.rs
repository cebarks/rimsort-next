use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    process::ExitCode,
};

fn run() -> io::Result<u8> {
    let exe = std::env::current_exe()?;
    let plans = exe
        .parent()
        .ok_or_else(|| io::Error::other("missing fixture directory"))?;
    let root = plans
        .parent()
        .ok_or_else(|| io::Error::other("missing fixture root"))?;
    let mut request = Vec::new();
    io::stdin()
        .take(1024 * 1024 + 1)
        .read_to_end(&mut request)?;
    if request.len() > 1024 * 1024 {
        return Err(io::Error::other("request exceeds 1 MiB"));
    }
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("requests"))?;
    log.write_all(&request)?;
    log.write_all(b"\n")?;
    for entry in fs::read_dir(plans)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let batch = entry.path();
        if fs::read(batch.join("request"))? != request {
            continue;
        }
        let exit_code = fs::read_to_string(batch.join("exit-code"))?
            .parse::<u8>()
            .map_err(io::Error::other)?;
        io::stdout()
            .lock()
            .write_all(&fs::read(batch.join("stdout"))?)?;
        return Ok(exit_code);
    }
    Err(io::Error::other("unplanned request"))
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("native acquisition fixture: {error}");
            ExitCode::FAILURE
        }
    }
}
