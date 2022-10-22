#![feature(generic_associated_types)]
#![feature(type_alias_impl_trait)]

fn main() {
    println!("Hello, world!");
}

#[gat_trait::gat_trait]
trait Animal {
    async fn run(&self);
}

struct Dog;

#[gat_trait::gat_trait]
impl Animal for Dog {
    async fn run(&self) {
        todo!()
    }
}