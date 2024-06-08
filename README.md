# `winmsg-executor`

Per-thread async rust executor for windows.
Each task is backed by a [message-only window][1].
The executor thread runs the native [windows message loop][2] which dispatches wake messages to the tasks window procedure which polls the task future.

## Features

- Easy data sharing within a thread because `Send` or `Sync` is not required for the task future.
- Runs multiply tasks on the same thread. Tasks can spawn new tasks and await the result.
- Modal windows like menus do not block other tasks running on the same thread.
- Helper code to implement window procedures with closures that can have state.

## Alternative Backend: `async-task`

Selected by the `backend-async-task` cargo feature.
Uses `async-task`s task abstraction instead of a window per task to store the future.
Scheduling a tasks means posting its runnable to the threads message queue (similar to `windows-executor` see below).

## Comparison with similar crates

Both of those listed crates run one taks/future per thread in their and expose
only `block_on()`.
[Is block_on an executor?](https://github.com/rust-lang/async-book/issues/219)

### [`windows-exeuctor`](https://github.com/haileys/windows-executor/)

- Polls its future directly from the message loop.
- Does not create a windows at all: Waker stores the message loops thread id and notifies it with `PostThreadMessage()`.
- Does not close the threads message loop (no `PostQuitMessage()` call) when the task futures returns.

### [`windows-async-rs`](https://github.com/saelay/windows-async-rs/)

- Polls directly from the message loop even when receiving broadcast messages unrelated to the task.
- Questionable use of unsafe code

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
