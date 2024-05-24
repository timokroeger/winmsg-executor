use std::{future::poll_fn, task::Poll, thread};

use winmsg_executor::MessageLoop;

async fn poll_n_times(mut n_poll: usize) {
    poll_fn(|cx| {
        println!("n_poll={n_poll}");
        if n_poll == 0 {
            Poll::Ready(())
        } else {
            n_poll -= 1;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    })
    .await;
}

fn main() {
    thread::spawn(|| {
        println!("thread hello");
        let msg_loop = MessageLoop::new();
        msg_loop.block_on(async {
            println!("thread async hello");
            poll_n_times(3).await;
            println!("thread async bye");
        });
        println!("thread bye");
    });

    println!("main hello");
    let msg_loop = MessageLoop::new();
    msg_loop.block_on(async {
        println!("main async hello");
        poll_n_times(3).await;
        println!("main async bye");
    });
    println!("main bye");
}
