use std::{future::poll_fn, task::Poll};

use winmsg_executor::Executor;

async fn poll_n_times(mut n_poll: usize) {
    poll_fn(|cx| {
        println!("n_poll={n_poll}");
        if n_poll == 0 {
            Poll::Ready(())
        } else {
            n_poll -= 1;
            cx.waker().clone().wake();
            Poll::Pending
        }
    })
    .await;
}

fn main() {
    println!("hello");
    Executor::run(|spawner| {
        spawner.spawn(async {
            println!("async hello 1");
            poll_n_times(3).await;
            println!("async bye 1");
        });
        spawner.spawn(async {
            println!("async hello 2");
            poll_n_times(2).await;
            println!("async bye 2");
        });
    });

    println!("bye");
}
