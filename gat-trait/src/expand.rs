use crate::lifetime::{AddLifetimeToImplTrait, CollectLifetimes};
use crate::parse::Item;
use crate::receiver::{has_self_in_block, has_self_in_sig, mut_pat, ReplaceSelf};
use heck::ToUpperCamelCase;
use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote, quote_spanned, ToTokens};
use std::collections::BTreeSet as Set;
use std::mem;
use syn::punctuated::Punctuated;
use syn::visit_mut::{self, VisitMut};
use syn::{
    parse_quote, parse_quote_spanned, Attribute, Block, FnArg, GenericParam, Generics, Ident,
    ImplItem, Lifetime, LifetimeDef, Pat, PatIdent, Receiver, ReturnType, Signature, Stmt, Token,
    TraitItem, Type, TypeParamBound, TypePath, WhereClause,
};
use syn::parse_quote::ParseQuote;

impl ToTokens for Item {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        match self {
            Item::Trait(item) => item.to_tokens(tokens),
            Item::Impl(item) => item.to_tokens(tokens),
        }
    }
}

#[derive(Clone, Copy)]
enum Context<'a> {
    Trait {
        generics: &'a Generics,
        super_traits: &'a SuperTraits,
    },
    Impl {
        impl_generics: &'a Generics,
        associated_type_impl_traits: &'a Set<Ident>,
    },
}

impl Context<'_> {
    fn lifetimes<'a>(&'a self, used: &'a [Lifetime]) -> impl Iterator<Item = &'a LifetimeDef> {
        let generics = match self {
            Context::Trait { generics, .. } => generics,
            Context::Impl { impl_generics, .. } => impl_generics,
        };
        generics.params.iter().filter_map(move |param| {
            if let GenericParam::Lifetime(param) = param {
                if used.contains(&param.lifetime) {
                    return Some(param);
                }
            }
            None
        })
    }
}

type SuperTraits = Punctuated<TypeParamBound, Token![+]>;

pub fn expand(input: &mut Item, is_local: bool) {
    match input {
        Item::Trait(input) => {
            let context = Context::Trait {
                generics: &input.generics,
                super_traits: &input.supertraits,
            };
            let mut items = Vec::new();
            for inner in &mut input.items {
                if let TraitItem::Method(method) = inner {
                    let sig = &mut method.sig;
                    if sig.asyncness.is_some() {
                        let block = &mut method.default;
                        let mut has_self = has_self_in_sig(sig);
                        method.attrs.push(parse_quote!(#[must_use]));
                        if let Some(block) = block {
                            has_self |= has_self_in_block(block);
                            transform_block(context, sig, block);
                            method.attrs.push(lint_suppress_with_body());
                        } else {
                            method.attrs.push(lint_suppress_without_body());
                        }
                        let has_default = method.default.is_some();
                        items.push(transform_sig(context, sig, has_self, has_default, is_local));
                    }
                }
            }
            for trait_item_type in items {
                input.items.push(TraitItem::Type(trait_item_type));
            }
        }
        Item::Impl(input) => {
            let mut lifetimes = CollectLifetimes::new("'impl", input.impl_token.span);
            lifetimes.visit_type_mut(&mut *input.self_ty);
            lifetimes.visit_path_mut(&mut input.trait_.as_mut().unwrap().1);
            let params = &input.generics.params;
            let elided = lifetimes.elided;
            input.generics.params = parse_quote!(#(#elided,)* #params);

            let mut associated_type_impl_traits = Set::new();
            for inner in &input.items {
                if let ImplItem::Type(assoc) = inner {
                    if let Type::ImplTrait(_) = assoc.ty {
                        associated_type_impl_traits.insert(assoc.ident.clone());
                    }
                }
            }

            let context = Context::Impl {
                impl_generics: &input.generics,
                associated_type_impl_traits: &associated_type_impl_traits,
            };
            let mut items = Vec::new();
            for inner in &mut input.items {
                if let ImplItem::Method(method) = inner {
                    let sig = &mut method.sig;
                    if sig.asyncness.is_some() {
                        let block = &mut method.block;
                        let has_self = has_self_in_sig(sig) || has_self_in_block(block);
                        transform_block(context, sig, block);
                        items.push(transform_sig(context, sig, has_self, false, is_local));
                        method.attrs.push(lint_suppress_with_body());
                    }
                }
            }
            for trait_item_type in items {
                input.items.push(ImplItem::Type(trait_item_type));
            }
        }
    }
}

fn lint_suppress_with_body() -> Attribute {
    parse_quote! {
        #[allow(
            clippy::let_unit_value,
            clippy::no_effect_underscore_binding,
            clippy::shadow_same,
            clippy::type_complexity,
            clippy::type_repetition_in_bounds,
            clippy::used_underscore_binding
        )]
    }
}

fn lint_suppress_without_body() -> Attribute {
    parse_quote! {
        #[allow(
            clippy::type_complexity,
            clippy::type_repetition_in_bounds
        )]
    }
}

// Input:
//     async fn f<T>(&self, x: &T) -> Ret;
//
// Output:
//     fn f<'life0, 'life1, 'gat_trait, T>(
//         &'life0 self,
//         x: &'life1 T,
//     ) -> Self::FResultFuture<'gat_trait>
//     where
//         'life0: 'gat_trait,
//         'life1: 'gat_trait,
//         T: 'gat_trait,
//         Self: Sync + 'gat_trait;
fn transform_sig<T: ParseQuote>(
    context: Context,
    sig: &mut Signature,
    has_self: bool,
    has_default: bool,
    is_local: bool,
) -> T {
    let default_span = sig.asyncness.take().unwrap().span;
    sig.fn_token.span = default_span;

    let (ret_arrow, ret) = match &sig.output {
        ReturnType::Default => (Token![->](default_span), quote_spanned!(default_span=> ())),
        ReturnType::Type(arrow, ret) => (*arrow, quote!(#ret)),
    };

    let mut lifetimes = CollectLifetimes::new("'life", default_span);
    for arg in sig.inputs.iter_mut() {
        match arg {
            FnArg::Receiver(arg) => lifetimes.visit_receiver_mut(arg),
            FnArg::Typed(arg) => lifetimes.visit_type_mut(&mut arg.ty),
        }
    }

    for param in &mut sig.generics.params {
        match param {
            GenericParam::Type(param) => {
                let param_name = &param.ident;
                let span = match param.colon_token.take() {
                    Some(colon_token) => colon_token.span,
                    None => param_name.span(),
                };
                let bounds = mem::replace(&mut param.bounds, Punctuated::new());
                where_clause_or_default(&mut sig.generics.where_clause)
                    .predicates
                    .push(parse_quote_spanned!(span=> #param_name: 'gat_trait + #bounds));
            }
            GenericParam::Lifetime(param) => {
                let param_name = &param.lifetime;
                let span = match param.colon_token.take() {
                    Some(colon_token) => colon_token.span,
                    None => param_name.span(),
                };
                let bounds = mem::replace(&mut param.bounds, Punctuated::new());
                where_clause_or_default(&mut sig.generics.where_clause)
                    .predicates
                    .push(parse_quote_spanned!(span=> #param: 'gat_trait + #bounds));
            }
            GenericParam::Const(_) => {}
        }
    }

    for param in context.lifetimes(&lifetimes.explicit) {
        let param = &param.lifetime;
        let span = param.span();
        where_clause_or_default(&mut sig.generics.where_clause)
            .predicates
            .push(parse_quote_spanned!(span=> #param: 'gat_trait));
    }

    if sig.generics.lt_token.is_none() {
        sig.generics.lt_token = Some(Token![<](sig.ident.span()));
    }
    if sig.generics.gt_token.is_none() {
        sig.generics.gt_token = Some(Token![>](sig.paren_token.span));
    }

    for elided in lifetimes.elided {
        sig.generics.params.push(parse_quote!(#elided));
        where_clause_or_default(&mut sig.generics.where_clause)
            .predicates
            .push(parse_quote_spanned!(elided.span()=> #elided: 'gat_trait));
    }

    sig.generics
        .params
        .push(parse_quote_spanned!(default_span=> 'gat_trait));

    if has_self {
        let bound = match sig.inputs.iter().next() {
            Some(FnArg::Receiver(Receiver {
                reference: Some(_),
                mutability: None,
                ..
            })) => Ident::new("Sync", default_span),
            Some(FnArg::Typed(arg))
                if match (arg.pat.as_ref(), arg.ty.as_ref()) {
                    (Pat::Ident(pat), Type::Reference(ty)) => {
                        pat.ident == "self" && ty.mutability.is_none()
                    }
                    _ => false,
                } =>
            {
                Ident::new("Sync", default_span)
            }
            _ => Ident::new("Send", default_span),
        };

        let assume_bound = match context {
            Context::Trait { super_traits, .. } => !has_default || has_bound(super_traits, &bound),
            Context::Impl { .. } => true,
        };

        let where_clause = where_clause_or_default(&mut sig.generics.where_clause);
        where_clause.predicates.push(if assume_bound || is_local {
            parse_quote_spanned!(default_span=> Self: 'gat_trait)
        } else {
            parse_quote_spanned!(default_span=> Self: ::core::marker::#bound + 'gat_trait)
        });
    }

    for (i, arg) in sig.inputs.iter_mut().enumerate() {
        match arg {
            FnArg::Receiver(Receiver {
                reference: Some(_), ..
            }) => {}
            FnArg::Receiver(arg) => arg.mutability = None,
            FnArg::Typed(arg) => {
                if let Pat::Ident(ident) = &mut *arg.pat {
                    ident.by_ref = None;
                    ident.mutability = None;
                } else {
                    let positional = positional_arg(i, &arg.pat);
                    let m = mut_pat(&mut arg.pat);
                    arg.pat = parse_quote!(#m #positional);
                }
                AddLifetimeToImplTrait.visit_type_mut(&mut arg.ty);
            }
        }
    }

    let bound = quote_spanned!(default_span=> 'gat_trait);
    let bounds = if is_local {
        bound.clone()
    } else {
        quote_spanned!(default_span=> ::core::marker::Send + 'gat_trait)
    };
    let ret_fut_name = upper_camel_case_ret_future(&sig.ident);
    sig.output = parse_quote_spanned! {default_span=>
        #ret_arrow Self::#ret_fut_name<#bound>
    };

    match context {
        Context::Trait {..} => parse_quote!(
            type #ret_fut_name<#bound>: ::core::future::Future<Output = #ret> + #bounds
            where
                Self: #bound;
        ),
        Context::Impl {..} => parse_quote!(
            type #ret_fut_name<#bound> = impl ::core::future::Future<Output = #ret> + #bounds;
        )
    }
}

// Input:
//     async fn f<T>(&self, x: &T, (a, b): (A, B)) -> Ret {
//         self + x + a + b
//     }
//
// Output:
//     async move {
//         let ___ret: Ret = {
//             let __self = self;
//             let x = x;
//             let (a, b) = __arg1;
//
//             __self + x + a + b
//         };
//
//         ___ret
//     }
fn transform_block(context: Context, sig: &mut Signature, block: &mut Block) {
    if let Some(Stmt::Item(syn::Item::Verbatim(item))) = block.stmts.first() {
        if block.stmts.len() == 1 && item.to_string() == ";" {
            return;
        }
    }

    let mut self_span = None;
    let decls = sig
        .inputs
        .iter()
        .enumerate()
        .map(|(i, arg)| match arg {
            FnArg::Receiver(Receiver {
                self_token,
                mutability,
                ..
            }) => {
                let ident = Ident::new("__self", self_token.span);
                self_span = Some(self_token.span);
                quote!(let #mutability #ident = #self_token;)
            }
            FnArg::Typed(arg) => {
                if let Pat::Ident(PatIdent {
                    ident, mutability, ..
                }) = &*arg.pat
                {
                    if ident == "self" {
                        self_span = Some(ident.span());
                        let prefixed = Ident::new("__self", ident.span());
                        quote!(let #mutability #prefixed = #ident;)
                    } else {
                        quote!(let #mutability #ident = #ident;)
                    }
                } else {
                    let pat = &arg.pat;
                    let ident = positional_arg(i, pat);
                    if let Pat::Wild(_) = **pat {
                        quote!(let #ident = #ident;)
                    } else {
                        quote!(let #pat = #ident;)
                    }
                }
            }
        })
        .collect::<Vec<_>>();

    if let Some(span) = self_span {
        let mut replace_self = ReplaceSelf(span);
        replace_self.visit_block_mut(block);
    }

    let stmts = &block.stmts;
    let let_ret = match &mut sig.output {
        ReturnType::Default => quote_spanned! {block.brace_token.span=>
            #(#decls)*
            let _: () = { #(#stmts)* };
        },
        ReturnType::Type(_, ret) => {
            if contains_associated_type_impl_trait(context, ret) {
                if decls.is_empty() {
                    quote!(#(#stmts)*)
                } else {
                    quote!(#(#decls)* { #(#stmts)* })
                }
            } else {
                quote_spanned! {block.brace_token.span=>
                    if let ::core::option::Option::Some(__ret) = ::core::option::Option::None::<#ret> {
                        return __ret;
                    }
                    #(#decls)*
                    let __ret: #ret = { #(#stmts)* };
                    #[allow(unreachable_code)]
                    __ret
                }
            }
        }
    };
    let async_stmt = quote_spanned!(block.brace_token.span=>
        async move { #let_ret }
    );
    block.stmts = parse_quote!(#async_stmt);
}

fn positional_arg(i: usize, pat: &Pat) -> Ident {
    let span: Span = syn::spanned::Spanned::span(pat);
    #[cfg(not(no_span_mixed_site))]
    let span = span.resolved_at(Span::mixed_site());
    format_ident!("__arg{}", i, span = span)
}

fn has_bound(super_traits: &SuperTraits, marker: &Ident) -> bool {
    for bound in super_traits {
        if let TypeParamBound::Trait(bound) = bound {
            if bound.path.is_ident(marker)
                || bound.path.segments.len() == 3
                    && (bound.path.segments[0].ident == "std"
                        || bound.path.segments[0].ident == "core")
                    && bound.path.segments[1].ident == "marker"
                    && bound.path.segments[2].ident == *marker
            {
                return true;
            }
        }
    }
    false
}

fn contains_associated_type_impl_trait(context: Context, ret: &mut Type) -> bool {
    struct AssociatedTypeImplTraits<'a> {
        set: &'a Set<Ident>,
        contains: bool,
    }

    impl<'a> VisitMut for AssociatedTypeImplTraits<'a> {
        fn visit_type_path_mut(&mut self, ty: &mut TypePath) {
            if ty.qself.is_none()
                && ty.path.segments.len() == 2
                && ty.path.segments[0].ident == "Self"
                && self.set.contains(&ty.path.segments[1].ident)
            {
                self.contains = true;
            }
            visit_mut::visit_type_path_mut(self, ty);
        }
    }

    match context {
        Context::Trait { .. } => false,
        Context::Impl {
            associated_type_impl_traits,
            ..
        } => {
            let mut visit = AssociatedTypeImplTraits {
                set: associated_type_impl_traits,
                contains: false,
            };
            visit.visit_type_mut(ret);
            visit.contains
        }
    }
}

fn where_clause_or_default(clause: &mut Option<WhereClause>) -> &mut WhereClause {
    clause.get_or_insert_with(|| WhereClause {
        where_token: Default::default(),
        predicates: Punctuated::new(),
    })
}

fn upper_camel_case_ret_future(func: &Ident) -> Ident {
    let fname = format!("{}_result_future", func.to_string());
    let fname = fname.to_upper_camel_case();
    Ident::new(&fname, Span::call_site())
}
