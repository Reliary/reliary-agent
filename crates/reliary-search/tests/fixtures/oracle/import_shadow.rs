use tokio::task;

fn run() {
    fn task(x: usize) -> usize {
        x + 1
    }
    let _ = task(1);
}
