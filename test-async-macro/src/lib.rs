extern crate proc_macro;
use proc_macro::TokenStream;

#[proc_macro_attribute]
pub fn test_async(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(item as syn::ItemFn);

    let ret = &input.sig.output;
    let name = &input.sig.ident;
    let body = &input.block;
    let attrs = &input.attrs;
    let vis = &input.vis;

    let out = quote::quote! {
        #[::core::prelude::v1::test]
        #(#attrs)*
        #vis fn #name() #ret {
            ::async_std::task::block_on(async { #body });
            ::tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async { #body });
        }
    };

    // let name = quote::format_ident!("{}", attr.to_string());
    // let input = proc_macro2::TokenStream::from(item);
    // let out = quote::quote! {
    //     #[test]
    //     fn #name() {
    //         #input

    //         ::async_std::task::block_on(#name());
    //         ::tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(#name());
    //     }
    // };
    out.into()
}
