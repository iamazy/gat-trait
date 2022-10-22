use proc_macro2::Span;
use syn::parse::{Error, Parse, ParseStream, Result};
use syn::Token;

#[derive(Copy, Clone)]
pub struct Args {
    pub local: bool,
}

mod kw {
    syn::custom_keyword!(Send);
}

impl Parse for Args {
    fn parse(input: ParseStream) -> Result<Self> {
        match try_parse(input) {
            Ok(args) if input.is_empty() => Ok(args),
            _ => Err(error()),
        }
    }
}

fn try_parse(input: ParseStream) -> Result<Args> {
    if input.peek(Token![?]) {
        input.parse::<Token![?]>()?;
        input.parse::<kw::Send>()?;
        Ok(Args { local: true })
    } else {
        Ok(Args { local: false })
    }
}

fn error() -> Error {
    let msg = "expected #[gat_trait] or #[gat_trait(?Send)]";
    Error::new(Span::call_site(), msg)
}
