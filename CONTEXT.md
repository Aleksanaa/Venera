# Venera Debug Channel Domain Model

This file defines the debug channel vocabulary. See the [guest service](docs/virtio-socket-guest.md) and [host workbench](docs/virtio-socket-host.md) for operation and deployment.

| Term | Meaning |
|---|---|
| Log producer | A component, driver, service, or helper process that produces diagnostic records. Users set its debug verbosity through component configuration, determining the actual output volume. |
| Session helper | A process that gathers system information in a particular Windows logon session, including local and RDP sessions. |
| Machine aggregator | The continuously running guest background process that aggregates session and system records, listens for VirtIO socket connections, and maintains the authoritative reception filter. |
| Host receiver | The host background process that continuously receives and saves records and provides queries and controls. |
| Host service | The background application that connects to a guest, continuously collects history, and provides the browser workbench and local controls. |
| Guest CID | The VM's VirtIO socket address, chosen for its deployment. Each running guest has a distinct CID above 2. |
| Peer connection | One host client's connection to the guest. Each peer receives its own stream of records; peers share the machine aggregator's effective filter. |
| Viewer | The panel, host interface, or CLI that consumes background state and records. |
| Reception filter | Selects already-produced records for forwarding and storage by component, existing level, and target. By default, it accepts all existing levels from the target components. |
| Process instance | One process lifetime identified by PID and process start time, optionally including its subsequently created descendants. |
| Program rule | Matches current and future process instances by the full executable path. |
| Effective configuration | The filter and expiration time actually used by the machine aggregator, available for both the panel and host to view and change. |
| Local forwarding switch | Forwarding state managed locally in Windows. Turning it off takes effect immediately; turning it back on is a local operation. |
| Temporary filter | A filter with a duration that reverts to the default on expiration. Its original expiration schedule continues across disconnections and viewer closure. |
| Observable gap | A known occurrence of discarded records, disconnection, or a parsing error. Exact counts depend on evidence from the corresponding collection layer. |
| Storage capacity budget | The adjustable capacity of the host history store. The oldest records are evicted when the budget is reached; users retain exported files. |
| History export range | Selects history by host reception time, including the start instant and treating the end instant as the start of the next range. |
