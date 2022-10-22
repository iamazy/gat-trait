#![allow(
    clippy::default_trait_access,
    clippy::doc_markdown,
    clippy::explicit_auto_deref,
    clippy::if_not_else,
    clippy::items_after_statements,
    clippy::module_name_repetitions,
    clippy::shadow_unrelated,
    clippy::similar_names,
    clippy::too_many_lines
)]

extern crate proc_macro;

mod args;
mod expand;
mod lifetime;
mod parse;
mod receiver;

use crate::args::Args;
use crate::expand::expand;
use crate::parse::Item;
use proc_macro::TokenStream;
use quote::quote;
use syn::parse_macro_input;

#[proc_macro_attribute]
pub fn gat_trait(args: TokenStream, input: TokenStream) -> TokenStream {
    let args = parse_macro_input!(args as Args);
    let mut item = parse_macro_input!(input as Item);
    expand(&mut item, args.local);
    TokenStream::from(quote!(#item))
}
