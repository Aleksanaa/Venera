use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

type Records = Arc<Mutex<Vec<Value>>>;
static SOURCES: OnceLock<Mutex<BTreeMap<PathBuf, Records>>> = OnceLock::new();

pub fn start(directory: &Path) {
    let mut sources = SOURCES.get_or_init(Default::default).lock().unwrap();
    if sources.contains_key(directory) {
        return;
    }
    let records = Arc::new(Mutex::new(Vec::new()));
    sources.insert(directory.to_owned(), records.clone());
    let directory = directory.to_owned();
    let listener = UnixListener::bind(directory.join("archive.sock")).unwrap();
    listener.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        while directory.exists() {
            match listener.accept() {
                Ok((stream, _)) => {
                    let _ = serve(stream, &records);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(_) => break,
            }
        }
    });
}

pub fn capture(stream: &UnixStream, value: &Value) {
    let Ok(address) = stream.local_addr() else {
        return;
    };
    let Some(directory) = address.as_pathname().and_then(Path::parent) else {
        return;
    };
    let sources = SOURCES.get_or_init(Default::default).lock().unwrap();
    let Some(records) = sources.get(directory) else {
        return;
    };
    fn append(records: &mut Vec<Value>, value: &Value) {
        if let Some(values) = value.as_array() {
            for value in values {
                append(records, value);
            }
            return;
        }
        let event = match value["method"].as_str() {
            Some("log") => json!({"type":"log","record":value["params"]}),
            Some("gap") => json!({"type":"gap","gap":value["params"]}),
            _ => return,
        };
        let cursor = json!({"journal_id":"fixture","position":(records.len()+1).to_string()});
        records.push(json!({"cursor":cursor,"boot_id":"fixture-boot","recorded_at_ms":"1","startup":true,"event":event}));
    }
    append(&mut records.lock().unwrap(), value);
}

fn serve(mut stream: UnixStream, records: &Records) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = serde_json::from_str(&line)?;
        let records = records.lock().unwrap();
        let range =
            json!({"journal_id":"fixture","oldest":"1","high_water":records.len().to_string()});
        let result = match request["method"].as_str() {
            Some("endpoint.info") => {
                json!({"role":"guest","software_version":"archive-fixture","protocol_version":"1.0.0"})
            }
            Some("rpc.discover") => {
                serde_json::from_str(include_str!("../../../protocol/v1/guest.openrpc.json"))
                    .unwrap()
            }
            Some("history.range") => range,
            Some("history.ack") => request["params"].clone(),
            Some("history.read") => {
                let after = request["params"]["after"]["position"]
                    .as_str()
                    .unwrap_or("0")
                    .parse::<usize>()
                    .unwrap();
                let through = request["params"]["through"]
                    .as_str()
                    .map(|s| s.parse::<usize>().unwrap())
                    .unwrap_or(records.len())
                    .min(records.len());
                let next = (after + 1).min(through);
                json!({"range":range,"records":&records[after..next],"missing":[],"next":{"journal_id":"fixture","position":next.to_string()}})
            }
            _ => continue,
        };
        drop(records);
        writeln!(
            stream,
            "{}",
            json!({"jsonrpc":"2.0","id":request["id"],"result":result})
        )?;
    }
}
