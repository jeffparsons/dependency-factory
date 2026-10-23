use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::parse::Parse;
use syn::{Data, DeriveInput, Field, Fields, Path, Token, Type, parse_macro_input};

#[proc_macro_derive(Singleton, attributes(factory))]
pub fn derive_singleton(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn expand(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let ident = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    let indexes = index_types(&input.attrs)?;

    let body = match &input.data {
        Data::Struct(s) => build_struct_body(&s.fields)?,
        Data::Enum(e) => {
            return Err(syn::Error::new_spanned(
                e.enum_token,
                "#[derive(Singleton)] is not supported on enums",
            ));
        }
        Data::Union(u) => {
            return Err(syn::Error::new_spanned(
                u.union_token,
                "#[derive(Singleton)] is not supported on unions",
            ));
        }
    };

    let register = (!indexes.is_empty()).then(|| {
        quote! {
            fn register(
                this: &::std::sync::Arc<Self>,
                builder: &mut ::dependency_factory::Builder<'_>,
            ) -> ::core::result::Result<(), ::dependency_factory::BuildError> {
                #(builder.index::<#indexes>(this.clone());)*
                ::core::result::Result::Ok(())
            }
        }
    });

    Ok(quote! {
        #[automatically_derived]
        impl #impl_generics ::dependency_factory::Singleton for #ident #ty_generics #where_clause {
            fn build(
                builder: &mut ::dependency_factory::Builder<'_>,
            ) -> ::core::result::Result<Self, ::dependency_factory::BuildError> {
                let _ = builder;
                ::core::result::Result::Ok(#body)
            }

            #register
        }
    })
}

/// The types named by `#[factory(index(...))]` attributes on the struct,
/// in the order written. Each becomes one `builder.index::<T>(...)` call
/// in the generated `register`.
fn index_types(attrs: &[syn::Attribute]) -> syn::Result<Vec<Type>> {
    let mut indexes = Vec::new();
    for attr in attrs {
        if !attr.path().is_ident("factory") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("index") {
                let content;
                syn::parenthesized!(content in meta.input);
                indexes.extend(content.parse_terminated(Type::parse, Token![,])?);
                Ok(())
            } else {
                Err(meta.error(
                    "unsupported `#[factory(...)]` argument on a struct; supported: `index(dyn Trait, ...)`",
                ))
            }
        })?;
    }
    Ok(indexes)
}

fn build_struct_body(fields: &Fields) -> syn::Result<TokenStream2> {
    match fields {
        Fields::Named(named) => {
            let mut lines = Vec::with_capacity(named.named.len());
            for field in &named.named {
                let name = field.ident.as_ref().expect("named field has ident");
                let expr = field_expr(field)?;
                lines.push(quote! { #name: #expr, });
            }
            Ok(quote! { Self { #(#lines)* } })
        }
        Fields::Unnamed(unnamed) => {
            let mut exprs = Vec::with_capacity(unnamed.unnamed.len());
            for field in &unnamed.unnamed {
                let expr = field_expr(field)?;
                exprs.push(quote! { #expr, });
            }
            Ok(quote! { Self ( #(#exprs)* ) })
        }
        Fields::Unit => Ok(quote! { Self }),
    }
}

/// How the derive fills one field, from the field's `#[factory(...)]`
/// attribute.
enum Source {
    /// `builder.get_or_build()`, for a field with no attribute.
    Singleton,
    /// `query = key_fn`: build the value for the key `key_fn` returns.
    Query(Path),
    /// `default`: `Default::default()`, for state that is not a
    /// dependency.
    Default,
    /// `build = build_fn`: whatever `build_fn` returns.
    Build(Path),
}

fn field_expr(field: &Field) -> syn::Result<TokenStream2> {
    let mut source: Option<(&'static str, Source)> = None;
    for attr in &field.attrs {
        if !attr.path().is_ident("factory") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            let (name, parsed) = if meta.path.is_ident("query") {
                ("query", Source::Query(meta.value()?.parse::<Path>()?))
            } else if meta.path.is_ident("default") {
                if meta.input.peek(Token![=]) {
                    return Err(meta.error(
                        "`default` takes no value; to fill a field with a function, use `build = path::to::build_fn`",
                    ));
                }
                ("default", Source::Default)
            } else if meta.path.is_ident("build") {
                ("build", Source::Build(meta.value()?.parse::<Path>()?))
            } else {
                return Err(meta.error(
                    "unsupported `#[factory(...)]` argument on a field; supported: `query = path::to::key_fn`, `default`, `build = path::to::build_fn`",
                ));
            };
            if let Some((previous, _)) = &source {
                return Err(meta.error(if *previous == name {
                    format!("`{name}` may only be specified once per field")
                } else {
                    format!("`{name}` conflicts with `{previous}`; a field takes only one of `query`, `default` and `build`")
                }));
            }
            source = Some((name, parsed));
            Ok(())
        })?;
    }

    Ok(
        match source.map_or(Source::Singleton, |(_, source)| source) {
            Source::Singleton => quote! { builder.get_or_build()? },
            Source::Query(path) => quote! {{
                let key = #path(builder)?;
                builder.get_or_build_for(key)?
            }},
            Source::Default => quote! { ::core::default::Default::default() },
            Source::Build(path) => quote! { #path(builder)? },
        },
    )
}
