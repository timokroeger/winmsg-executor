# winmsg-executor

Per-thread async rust executor for windows.
Each task is backed by a [message-only window][1].
The executor thread runs the native [windows message loop][2] which dispatches wake messages to the tasks window procedure which polls the task future.

As a thin layer around WinAPI calls the whole exeuctor is implemented in less than 250 lines of code.

## WIP Comparison with similar crates

https://github.com/haileys/windows-executor/tree/main

- ???

https://github.com/saelay/windows-async-rs/

- Only one top-level task

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the
work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any
additional terms or conditions.

[1]: https://learn.microsoft.com/en-us/windows/win32/winmsg/window-features#message-only-windows
[2]: https://learn.microsoft.com/en-us/windows/win32/winmsg/messages-and-message-queues
