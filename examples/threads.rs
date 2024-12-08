use std::{future::poll_fn, task::Poll, thread};

use winmsg_executor::block_on;

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
        block_on(async {
            println!("thread async hello");
            poll_n_times(3).await;
            println!("thread async bye");
        });
        println!("thread bye");
    });

    println!("main hello");
    block_on(async {
        println!("main async hello");
        poll_n_times(3).await;
        println!("main async bye");
    });
    println!("main bye");
}
