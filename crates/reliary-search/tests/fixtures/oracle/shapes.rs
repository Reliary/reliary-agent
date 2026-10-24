// Oracle fixture: Rust shapes incl. where-clauses (B7).
pub fn block_on(x: i32) -> i32 {
    x
}

pub struct Foo<T> {
    v: T,
}

pub struct Bar<T>
where
    T: Clone,
{
    v: T,
}

impl<T: Clone> Bar<T> {
    pub fn get(&self) -> &T {
        &self.v
    }
}
