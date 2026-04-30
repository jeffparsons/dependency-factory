use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Data, DeriveInput, Field, Fields, Path, parse_macro_input};

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

    Ok(quote! {
        #[automatically_derived]
        impl #impl_generics ::dependency_factory::Singleton for #ident #ty_generics #where_clause {
            fn build(
                factory: &::dependency_factory::DependencyFactoryHandle,
            ) -> ::core::result::Result<Self, ::dependency_factory::BuildError> {
                let _ = factory;
                ::core::result::Result::Ok(#body)
            }
        }
    })
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

fn field_expr(field: &Field) -> syn::Result<TokenStream2> {
    let mut query: Option<Path> = None;
    for attr in &field.attrs {
        if !attr.path().is_ident("factory") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("query") {
                if query.is_some() {
                    return Err(meta.error("`query` may only be specified once per field"));
                }
                query = Some(meta.value()?.parse::<Path>()?);
                Ok(())
            } else {
                Err(meta.error(
                    "unsupported `#[factory(...)]` argument; supported: `query = path::to::key_fn`",
                ))
            }
        })?;
    }

    Ok(match query {
        Some(path) => quote! { factory.build_for(#path(factory)?)? },
        None => quote! { factory.build()? },
    })
}
