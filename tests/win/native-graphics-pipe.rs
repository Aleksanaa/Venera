// SPDX-License-Identifier: MIT
use super::*;

pub(crate) fn read(state: Shared, params: serde_json::Value) -> serde_json::Value {
    let (server, mut file) = helper_pair();
    let mut owner = PipeWorkers::new();
    let stop = owner.local_stop.clone();
    owner.threads.push(std::thread::spawn(move || {
        let _ = client(server, state, &mut false, &stop);
    }));
    let mut rpc = rpc_negotiate(&mut file).unwrap();
    let result = rpc_call(&mut rpc, &mut file, venera_debug_protocol::v1::Method::HistoryRead, params).unwrap();
    drop(file);
    drop(owner);
    result
}
