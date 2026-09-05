use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    sync::mpsc::{Receiver, Sender, channel},
    thread,
    time::Duration,
};

use crate::{Direction, ResizeDirection, WorkspaceId};

const MAX_REQUEST_BYTES: u64 = 4 * 1024;
const REQUEST_READ_TIMEOUT: Duration = Duration::from_millis(250);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);
const CLIENT_TIMEOUT: Duration = Duration::from_secs(6);

pub const COMMAND_USAGE: &str = "status|debug|switch WORKSPACE|back-and-forth|move WORKSPACE|move-app ID WORKSPACE|focus DIR|move-direction DIR|layout|resize shrink|resize grow|balance|pause|resume|retile|restart|quit";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    Status,
    Debug,
    Switch(WorkspaceId),
    WorkspaceBackAndForth,
    Move(WorkspaceId),
    MoveApp(String, WorkspaceId),
    Focus(Direction),
    MoveDirection(Direction),
    Layout,
    Resize(ResizeDirection),
    Balance,
    Pause,
    Resume,
    Retile,
    Restart,
    Quit,
}

pub struct Request {
    pub command: Command,
    response: Sender<Result<String, String>>,
}

impl Request {
    pub fn respond(self, response: Result<String, String>) {
        let _ = self.response.send(response);
    }
}

pub struct Server {
    path: PathBuf,
    _lock: File,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn listen(requests: Sender<Request>) -> Result<Server, String> {
    let path = socket_path()?;
    listen_at(path, requests)
}

fn listen_at(path: PathBuf, requests: Sender<Request>) -> Result<Server, String> {
    let parent = path.parent().ok_or("control socket path has no parent")?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create control socket directory: {error}"))?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("failed to secure control socket directory: {error}"))?;
    let lock_path = path.with_extension("lock");
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(&lock_path)
        .map_err(|error| format!("failed to open control socket lock: {error}"))?;
    lock.try_lock()
        .map_err(|_| "another Spaceman agent is already starting or running".to_string())?;
    fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("failed to secure control socket lock: {error}"))?;
    if UnixStream::connect(&path).is_ok() {
        return Err("another Spaceman agent is already running".into());
    }
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("failed to remove stale control socket: {error}")),
    }
    let listener = UnixListener::bind(&path)
        .map_err(|error| format!("failed to bind control socket: {error}"))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("failed to secure control socket: {error}"))?;
    if let Err(error) = thread::Builder::new()
        .name("spaceman-control".into())
        .spawn(move || serve(listener, requests))
    {
        let _ = fs::remove_file(&path);
        return Err(format!("failed to start control server: {error}"));
    }
    Ok(Server { path, _lock: lock })
}

pub fn send(arguments: &[String]) -> Result<String, String> {
    let command = arguments.join(" ");
    parse_line(&command)?;
    let path = socket_path()?;
    let mut stream =
        UnixStream::connect(&path).map_err(|_| "Spaceman agent is not running".to_string())?;
    stream
        .set_write_timeout(Some(CLIENT_TIMEOUT))
        .map_err(|error| format!("failed to configure control command: {error}"))?;
    stream
        .set_read_timeout(Some(CLIENT_TIMEOUT))
        .map_err(|error| format!("failed to configure control response: {error}"))?;
    writeln!(stream, "{command}")
        .map_err(|error| format!("failed to send control command: {error}"))?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|error| format!("failed to finish control command: {error}"))?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|error| format!("failed to read control response: {error}"))?;
    if let Some(value) = response.strip_prefix("ok\n") {
        Ok(value.trim_end().to_string())
    } else if let Some(error) = response.strip_prefix("error\n") {
        Err(error.trim_end().to_string())
    } else {
        Err("Spaceman returned an invalid control response".into())
    }
}

fn serve(listener: UnixListener, requests: Sender<Request>) {
    for connection in listener.incoming() {
        let Ok(mut stream) = connection else {
            continue;
        };
        let _ = stream.set_read_timeout(Some(REQUEST_READ_TIMEOUT));
        let _ = stream.set_write_timeout(Some(REQUEST_READ_TIMEOUT));
        let response = match read_command(&stream) {
            Ok(line) => parse_line(line.trim()).and_then(|command| {
                let (response, receiver) = channel();
                requests
                    .send(Request { command, response })
                    .map_err(|_| "Spaceman agent is stopping".to_string())?;
                receive_response(receiver)
            }),
            Err(error) => Err(format!("failed to read control command: {error}")),
        };
        let (prefix, message) = match response {
            Ok(message) => ("ok", message),
            Err(error) => ("error", error),
        };
        let _ = writeln!(stream, "{prefix}\n{message}");
    }
}

fn read_command(reader: impl Read) -> Result<String, String> {
    let mut bytes = Vec::new();
    BufReader::new(reader)
        .take(MAX_REQUEST_BYTES + 1)
        .read_until(b'\n', &mut bytes)
        .map_err(|error| format!("failed to read control command: {error}"))?;
    if bytes.len() as u64 > MAX_REQUEST_BYTES {
        return Err("control command is too large".into());
    }
    if !bytes.ends_with(b"\n") {
        return Err("control command must end with a newline".into());
    }
    String::from_utf8(bytes).map_err(|_| "control command is not valid UTF-8".into())
}

fn receive_response(receiver: Receiver<Result<String, String>>) -> Result<String, String> {
    receiver
        .recv_timeout(RESPONSE_TIMEOUT)
        .map_err(|_| "Spaceman did not respond to the control command".to_string())?
}

fn socket_path() -> Result<PathBuf, String> {
    crate::config::application_support_path("control.sock").ok_or("HOME is unavailable".into())
}

fn parse_line(line: &str) -> Result<Command, String> {
    let parts = line.split_whitespace().collect::<Vec<_>>();
    match parts.as_slice() {
        ["status"] => Ok(Command::Status),
        ["debug"] => Ok(Command::Debug),
        ["switch", workspace] => parse_workspace(workspace).map(Command::Switch),
        ["back-and-forth"] => Ok(Command::WorkspaceBackAndForth),
        ["move", workspace] => parse_workspace(workspace).map(Command::Move),
        ["move-app", bundle_id, workspace] => parse_workspace(workspace)
            .map(|workspace| Command::MoveApp((*bundle_id).into(), workspace)),
        ["focus", direction] => direction.parse().map(Command::Focus),
        ["move-direction", direction] => direction.parse().map(Command::MoveDirection),
        ["layout"] => Ok(Command::Layout),
        ["resize", "shrink"] => Ok(Command::Resize(ResizeDirection::Shrink)),
        ["resize", "grow"] => Ok(Command::Resize(ResizeDirection::Grow)),
        ["balance"] => Ok(Command::Balance),
        ["pause"] => Ok(Command::Pause),
        ["resume"] => Ok(Command::Resume),
        ["retile"] => Ok(Command::Retile),
        ["restart"] => Ok(Command::Restart),
        ["quit"] => Ok(Command::Quit),
        _ => Err(format!("expected {COMMAND_USAGE}")),
    }
}

fn parse_workspace(value: &str) -> Result<WorkspaceId, String> {
    WorkspaceId::parse(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn parses_control_commands() {
        assert_eq!(parse_line("status").unwrap(), Command::Status);
        assert_eq!(
            parse_line("back-and-forth").unwrap(),
            Command::WorkspaceBackAndForth
        );
        assert_eq!(
            parse_line("switch 10").unwrap(),
            Command::Switch(WorkspaceId::new(10).unwrap())
        );
        assert!(parse_line("move 11").is_err());
        assert_eq!(
            parse_line("switch D").unwrap(),
            Command::Switch(WorkspaceId::named('D').unwrap())
        );
        assert_eq!(
            parse_line("move-app com.example.App 3").unwrap(),
            Command::MoveApp("com.example.App".into(), WorkspaceId::new(3).unwrap())
        );
        assert!(parse_line("resume now").is_err());
        assert_eq!(parse_line("restart").unwrap(), Command::Restart);
        assert_eq!(
            parse_line("focus left").unwrap(),
            Command::Focus(Direction::Left)
        );
        assert_eq!(
            parse_line("move-direction right").unwrap(),
            Command::MoveDirection(Direction::Right)
        );
        assert_eq!(parse_line("layout").unwrap(), Command::Layout);
        assert_eq!(
            parse_line("resize shrink").unwrap(),
            Command::Resize(ResizeDirection::Shrink)
        );
        assert_eq!(
            parse_line("resize grow").unwrap(),
            Command::Resize(ResizeDirection::Grow)
        );
        assert_eq!(parse_line("balance").unwrap(), Command::Balance);
    }

    #[test]
    fn control_socket_lock_prevents_a_second_server_from_removing_the_first_socket() {
        let path = temporary_socket_path("lock");
        let (requests, _receiver) = channel();
        let server = listen_at(path.clone(), requests).unwrap();
        let (requests, _receiver) = channel();

        assert!(listen_at(path.clone(), requests).is_err());
        assert!(UnixStream::connect(&path).is_ok());
        let lock_path = path.with_extension("lock");
        assert_eq!(
            fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        for secured in [&path, &lock_path] {
            assert_eq!(
                fs::metadata(secured).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        drop(server);
        let _ = fs::remove_file(lock_path);
        let _ = fs::remove_dir(path.parent().unwrap());
    }

    #[test]
    fn partial_request_read_is_bounded() {
        let (mut client, server) = UnixStream::pair().unwrap();
        server
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        client.write_all(b"sta").unwrap();
        let error = read_command(server).unwrap_err();

        assert!(error.contains("failed to read control command"));
    }

    #[test]
    fn command_input_requires_a_newline_and_has_a_fixed_limit() {
        assert_eq!(read_command(&b"status\n"[..]).unwrap(), "status\n");
        assert_eq!(
            read_command(&b"status"[..]).unwrap_err(),
            "control command must end with a newline"
        );
        let oversized = vec![b'x'; usize::try_from(MAX_REQUEST_BYTES + 1).unwrap()];
        assert_eq!(
            read_command(oversized.as_slice()).unwrap_err(),
            "control command is too large"
        );
    }

    fn temporary_socket_path(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir()
            .join(format!("spaceman-{label}-{}-{nonce}", std::process::id()))
            .join("control.sock")
    }
}
