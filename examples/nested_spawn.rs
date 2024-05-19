use std::future;

use winmsg_executor::Executor;

fn main() {
    println!("hello");
    Executor::run(|spawner| {
        let cloned_spawner = spawner.clone();
        spawner.spawn(async move {
            println!("async hello outer");
            cloned_spawner.spawn(async {
                println!("async hello inner");
                future::ready(()).await;
                println!("async bye inner");
            });
            future::ready(()).await;
            println!("async bye outer");
        });
    });
    println!("bye");
}
