mod native;

use std::{
    io::{self, Write},
    process::ExitCode,
    time::{Duration, Instant},
};
use steam_helper::Machine;
use steam_protocol::{Event, Facts, Outcome, Payload, Phase, Request, encode_event, read_request};

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && args[0] == "--version" {
        println!("steam-helper {} protocol=1", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if !args.is_empty() {
        eprintln!(
            "usage: steam-helper [--version]; otherwise read one JSON request to EOF on stdin"
        );
        return ExitCode::from(2);
    }
    let request = match read_request(io::stdin().lock()) {
        Ok(request) => request,
        Err(error) => {
            eprintln!("invalid request: {error}");
            return ExitCode::from(2);
        }
    };
    match run(request) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("helper failed: {error}");
            ExitCode::from(1)
        }
    }
}

fn output(writer: &mut impl Write, event: &Event) -> io::Result<()> {
    let line = encode_event(event).map_err(io::Error::other)?;
    writer.write_all(&line)?;
    writer.flush()
}

fn run(request: Request) -> Result<(), Box<dyn std::error::Error>> {
    let mut writer = io::stdout().lock();
    let mut api = match native::Native::initialize(&request) {
        Ok(api) => api,
        Err(error) => {
            unavailable(&mut writer, &request)?;
            return Err(io::Error::other(error).into());
        }
    };
    let mut machine = Machine::new(request, &mut api)?;
    let start = Instant::now();
    loop {
        for event in machine.take_events() {
            output(&mut writer, &event)?;
        }
        if machine.is_done() {
            break;
        }
        machine.tick(&mut api, start.elapsed() >= Duration::from_secs(120));
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn unavailable(writer: &mut impl Write, request: &Request) -> io::Result<()> {
    for &item_id in &request.ids {
        for phase in [Phase::Before, Phase::After] {
            output(
                writer,
                &Event::new(
                    &request.request_id,
                    Payload::Observation {
                        item_id,
                        phase,
                        facts: Facts::default(),
                    },
                ),
            )?;
        }
        output(
            writer,
            &Event::new(
                &request.request_id,
                Payload::Outcome {
                    item_id,
                    outcome: Outcome::Failed,
                },
            ),
        )?;
    }
    output(writer, &Event::new(&request.request_id, Payload::Terminal))
}

#[cfg(test)]
mod tests {
    use super::*;
    use steam_protocol::{Action, EventStream, read_line_bounded};
    #[test]
    fn initialization_failure_is_a_terminal_failed_operation_not_an_install() {
        let request = Request {
            schema: 1,
            app_id: 294100,
            request_id: "unavailable".into(),
            action: Action::Download,
            ids: vec![1, 2],
        };
        let mut bytes = Vec::new();
        unavailable(&mut bytes, &request).unwrap();
        let mut stream = EventStream::new(request).unwrap();
        let mut reader = io::Cursor::new(bytes);
        while let Some(line) = read_line_bounded(&mut reader).unwrap() {
            stream.accept_line(&line).unwrap();
        }
        stream.finish().unwrap();
        assert_eq!(
            stream
                .events()
                .iter()
                .filter(|event| matches!(
                    event.payload,
                    Payload::Outcome {
                        outcome: Outcome::Failed,
                        ..
                    }
                ))
                .count(),
            2
        );
    }
}
