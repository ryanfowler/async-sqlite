use proc_macro::TokenStream;

#[proc_macro_attribute]
pub fn test_async(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(item as syn::ItemFn);

    let ret = &input.sig.output;
    let name = &input.sig.ident;
    let body = &input.block;
    let attrs = &input.attrs;

    let out = quote::quote! {
        mod #name {
            use super::*;

            #(#attrs)*
            async fn func() #ret {
                #body
            }

            #[::core::prelude::v1::test]
            fn async_std() {
                ::async_std::task::block_on(func());
            }

            #[::core::prelude::v1::test]
            fn tokio() {
                ::tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(func());
            }
        }
    };

    out.into()
}
