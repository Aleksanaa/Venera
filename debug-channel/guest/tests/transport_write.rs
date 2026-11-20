#[path = "../src/windows/transport_write.rs"]
mod transport_write;

use std::{
    io,
    time::{Duration, Instant},
};
use transport_write::{send_error, PendingWrite, WriteStep};

fn frame() -> Vec<u8> {
    format!("{{\"text\":\"{}\"}}\n", "x".repeat(768 * 1024)).into_bytes()
}

fn assert_frame(bytes: &[u8]) {
    assert_eq!(bytes.last(), Some(&b'\n'));
    let value: serde_json::Value = serde_json::from_slice(bytes).unwrap();
    assert_eq!(value["text"], "x".repeat(786432));
}

#[test]
fn credit_exhaustion_resumes_the_complete_large_frame() {
    let now = Instant::now();
    let mut pending = PendingWrite::new(frame(), now);
    let mut received = Vec::new();
    while received.len() < 262144 {
        pending
            .write(now, true, |bytes| {
                let count = bytes.len().min(8192);
                received.extend_from_slice(&bytes[..count]);
                Ok(count)
            })
            .unwrap();
    }
    assert!(matches!(
        pending
            .write(now, true, |_| Err(send_error(0xc0000023u32 as i32)))
            .unwrap(),
        WriteStep::Blocked
    ));
    loop {
        let step = pending
            .write(now + Duration::from_millis(650), true, |bytes| {
                let count = bytes.len().min(1537);
                received.extend_from_slice(&bytes[..count]);
                Ok(count)
            })
            .unwrap();
        if matches!(step, WriteStep::Complete) {
            break;
        }
    }
    assert_frame(&received);
}

#[test]
fn continuing_short_writes_refresh_the_stall_deadline() {
    let start = Instant::now();
    let mut pending = PendingWrite::new(frame(), start);
    let mut received = Vec::new();
    for tick in 0..100 {
        let now = start + Duration::from_millis(tick * 500);
        let step = pending
            .write(now, true, |bytes| {
                let count = bytes.len().min(16384);
                received.extend_from_slice(&bytes[..count]);
                Ok(count)
            })
            .unwrap();
        if matches!(step, WriteStep::Complete) {
            break;
        }
        assert!(matches!(
            pending
                .write(now + Duration::from_millis(200), true, |_| Err(send_error(
                    10035
                )))
                .unwrap(),
            WriteStep::Blocked
        ));
    }
    assert_frame(&received);
}

#[test]
fn stopped_reader_expires_two_seconds_after_last_progress() {
    let start = Instant::now();
    let mut pending = PendingWrite::new(frame(), start);
    let mut received = Vec::new();
    pending
        .write(start + Duration::from_secs(1), true, |bytes| {
            received.extend_from_slice(&bytes[..17]);
            Ok(17)
        })
        .unwrap();
    assert!(matches!(
        pending
            .write(start + Duration::from_millis(2999), true, |_| Err(
                send_error(10035)
            ))
            .unwrap(),
        WriteStep::Blocked
    ));
    let result = pending.write(start + Duration::from_secs(3), true, |_| {
        Err(send_error(10035))
    });
    assert_eq!(result.err().unwrap().kind(), io::ErrorKind::TimedOut);
    assert_eq!(received, b"{\"text\":\"xxxxxxxx".to_vec());
}

#[test]
fn cancellation_stops_a_partially_written_frame() {
    let now = Instant::now();
    let mut pending = PendingWrite::new(frame(), now);
    let mut received = Vec::new();
    pending
        .write(now, true, |bytes| {
            received.extend_from_slice(&bytes[..9]);
            Ok(9)
        })
        .unwrap();
    let result = pending.write(now, false, |bytes| {
        received.extend_from_slice(bytes);
        Ok(bytes.len())
    });
    assert_eq!(result.err().unwrap().kind(), io::ErrorKind::Interrupted);
    assert_eq!(received, b"{\"text\":\"".to_vec());
}

#[test]
fn terminal_socket_errors_keep_their_original_code() {
    let now = Instant::now();
    let mut pending = PendingWrite::new(frame(), now);
    let error = pending
        .write(now, true, |_| Err(send_error(10054)))
        .err()
        .unwrap();
    assert_eq!(error.raw_os_error(), Some(10054));
    assert_eq!(
        pending.write(now, true, |_| Ok(0)).err().unwrap().kind(),
        io::ErrorKind::WriteZero
    );
}

#[cfg(unix)]
#[test]
fn slow_socket_reader_receives_the_complete_frame() {
    use std::{
        io::{Read, Write},
        os::unix::net::UnixStream,
        thread,
    };
    let (mut writer, mut reader) = UnixStream::pair().unwrap();
    writer.set_nonblocking(true).unwrap();
    let receive = thread::spawn(move || {
        let mut received = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let count = reader.read(&mut buffer).unwrap();
            if count == 0 {
                break;
            }
            received.extend_from_slice(&buffer[..count]);
            thread::sleep(Duration::from_millis(35));
        }
        received
    });
    let mut pending = PendingWrite::new(frame(), Instant::now());
    let result = loop {
        match pending.write(Instant::now(), true, |bytes| writer.write(bytes)) {
            Ok(WriteStep::Complete) => break Ok(()),
            Ok(WriteStep::Progress) => (),
            Ok(WriteStep::Blocked) => thread::sleep(Duration::from_millis(2)),
            Err(error) => break Err(error),
        }
    };
    drop(writer);
    let received = receive.join().unwrap();
    result.unwrap();
    assert_frame(&received);
}

#[cfg(unix)]
#[test]
fn stopped_socket_reader_releases_the_writer_after_its_stall_budget() {
    use std::{
        io::{Read, Write},
        os::unix::net::UnixStream,
        thread,
    };
    let (mut writer, mut reader) = UnixStream::pair().unwrap();
    writer.set_nonblocking(true).unwrap();
    let start = Instant::now();
    let mut pending = PendingWrite::new(frame(), start);
    let error = loop {
        match pending.write(Instant::now(), true, |bytes| writer.write(bytes)) {
            Ok(WriteStep::Complete) => panic!("test frame fitted entirely in the socket buffer"),
            Ok(WriteStep::Progress) => (),
            Ok(WriteStep::Blocked) => thread::sleep(Duration::from_millis(2)),
            Err(error) => break error,
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "writer stall deadline"
        );
    };
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    drop(writer);
    let mut received = Vec::new();
    reader.read_to_end(&mut received).unwrap();
    assert_eq!(&received[..9], b"{\"text\":\"");
    assert_eq!(received.last(), Some(&b'x'));
}
