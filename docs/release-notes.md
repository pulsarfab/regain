# PulsarFab regain 0.5.13.0

Maintenance release from the 0.5 branch.

- NINA and native ASCOM camera Driver Info now show live recovery state, the
  retry count, and the last failure. Counts distinguish replacement exposures,
  ready-frame download retries, and direct USB frame rereads.
- Counts reset for the next validated capture. The last failure remains visible
  after successful recovery, until disconnect. Retry budgets and exposure
  behavior are unchanged.
- Crate packaging verifies matching workspace versions together, preventing
  an older published core or another release train from being used by mistake.

Focused validation covers SDK and direct USB recovery, exhausted retries,
NINA live Driver Info notifications, and all four native ASCOM camera slots
in both 32-bit and 64-bit clients. All nine crate packages build successfully.
These recovery checks use explicit simulation; physical camera support is
unchanged from 0.5.12.0.

Use `Regain-ASCOM-0.5.13.0-win-x64-setup.exe` for the signed Windows installer,
or install `Regain-0.5.13.0.zip` through either public NINA plugin feed.
Minimum NINA version and plugin identity are unchanged. Rust workspace version
is 0.5.13. Windows release programs are signed by StackFoundry LLC.
