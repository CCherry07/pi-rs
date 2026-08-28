use proc_macro::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Ident, ImplItem, ItemImpl, LitStr, Token, parse_macro_input, parse_quote};

#[derive(Default)]
struct ExportArgs {
    factory: bool,
    id: Option<LitStr>,
}

impl Parse for ExportArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut args = Self::default();
        while !input.is_empty() {
            let name: Ident = input.parse()?;
            if name == "factory" {
                if args.factory {
                    return Err(syn::Error::new(name.span(), "duplicate `factory` option"));
                }
                args.factory = true;
            } else if name == "id" {
                input.parse::<Token![=]>()?;
                if args.id.is_some() {
                    return Err(syn::Error::new(name.span(), "duplicate `id` option"));
                }
                args.id = Some(input.parse()?);
            } else {
                return Err(syn::Error::new(
                    name.span(),
                    "expected `factory` or `id = \"...\"`",
                ));
            }
            if input.is_empty() {
                break;
            }
            input.parse::<Token![,]>()?;
        }
        Ok(args)
    }
}

#[proc_macro_attribute]
pub fn agent(args: TokenStream, item: TokenStream) -> TokenStream {
    export_plugin(args, item, PluginKind::Agent)
}

/// Prepares a statically linked `AgentPlugin` by deriving hook interests and
/// expanding async callback methods.
#[proc_macro_attribute]
pub fn agent_plugin(args: TokenStream, item: TokenStream) -> TokenStream {
    let args = proc_macro2::TokenStream::from(args);
    if !args.is_empty() {
        return syn::Error::new_spanned(args, "`agent_plugin` does not accept arguments")
            .into_compile_error()
            .into();
    }
    let mut implementation = parse_macro_input!(item as ItemImpl);
    match inject_agent_hook_interests(&mut implementation, quote!(::pi_core)) {
        Ok(()) => {
            ensure_async_trait(&mut implementation, quote!(::pi_core::__plugin_async_trait));
            quote!(#implementation).into()
        }
        Err(error) => error.into_compile_error().into(),
    }
}

/// Expands async callbacks for a statically linked `ProviderPlugin`.
#[proc_macro_attribute]
pub fn provider_plugin(args: TokenStream, item: TokenStream) -> TokenStream {
    expand_static_plugin(
        args,
        item,
        PluginKind::Provider,
        quote!(::pi_core::__plugin_async_trait),
    )
}

/// Expands async callbacks for a statically linked `SessionPlugin`.
#[proc_macro_attribute]
pub fn session_plugin(args: TokenStream, item: TokenStream) -> TokenStream {
    expand_static_plugin(
        args,
        item,
        PluginKind::Session,
        quote!(::pi_session::__plugin_async_trait),
    )
}

#[proc_macro_attribute]
pub fn provider(args: TokenStream, item: TokenStream) -> TokenStream {
    export_plugin(args, item, PluginKind::Provider)
}

#[proc_macro_attribute]
pub fn session(args: TokenStream, item: TokenStream) -> TokenStream {
    export_plugin(args, item, PluginKind::Session)
}

#[derive(Clone, Copy)]
enum PluginKind {
    Agent,
    Provider,
    Session,
}

fn expand_static_plugin(
    args: TokenStream,
    item: TokenStream,
    kind: PluginKind,
    async_trait_path: proc_macro2::TokenStream,
) -> TokenStream {
    let args = proc_macro2::TokenStream::from(args);
    if !args.is_empty() {
        return syn::Error::new_spanned(args, "static plugin attributes do not accept arguments")
            .into_compile_error()
            .into();
    }
    let mut implementation = parse_macro_input!(item as ItemImpl);
    let Some((_, trait_path, _)) = &implementation.trait_ else {
        return syn::Error::new_spanned(
            implementation,
            format!(
                "this attribute must annotate an impl of {}",
                kind.trait_name()
            ),
        )
        .into_compile_error()
        .into();
    };
    let actual_trait = trait_path
        .segments
        .last()
        .map(|segment| segment.ident.to_string())
        .unwrap_or_default();
    if actual_trait != kind.trait_name() {
        return syn::Error::new_spanned(
            trait_path,
            format!(
                "this attribute must annotate an impl of {}",
                kind.trait_name()
            ),
        )
        .into_compile_error()
        .into();
    }
    ensure_async_trait(&mut implementation, async_trait_path);
    quote!(#implementation).into()
}

impl PluginKind {
    fn trait_name(self) -> &'static str {
        match self {
            Self::Agent => "AgentPlugin",
            Self::Provider => "ProviderPlugin",
            Self::Session => "SessionPlugin",
        }
    }
}

fn export_plugin(args: TokenStream, item: TokenStream, kind: PluginKind) -> TokenStream {
    let args = parse_macro_input!(args as ExportArgs);
    let mut implementation = parse_macro_input!(item as ItemImpl);
    match expand_export(args, &mut implementation, kind) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.into_compile_error().into(),
    }
}

fn expand_export(
    args: ExportArgs,
    implementation: &mut ItemImpl,
    kind: PluginKind,
) -> syn::Result<proc_macro2::TokenStream> {
    let Some((_, trait_path, _)) = &implementation.trait_ else {
        return Err(syn::Error::new_spanned(
            implementation,
            "pi plugin export macros must annotate a plugin trait impl",
        ));
    };
    let actual_trait = trait_path
        .segments
        .last()
        .map(|segment| segment.ident.to_string())
        .unwrap_or_default();
    if actual_trait != kind.trait_name() {
        return Err(syn::Error::new_spanned(
            trait_path,
            format!("this macro must annotate an impl of {}", kind.trait_name()),
        ));
    }
    if matches!(kind, PluginKind::Agent) {
        inject_agent_hook_interests(implementation, quote!(::pi_plugin_sdk))?;
    }
    if implementation
        .items
        .iter()
        .any(|item| matches!(item, ImplItem::Fn(function) if function.sig.ident == "id"))
    {
        return Err(syn::Error::new_spanned(
            implementation,
            "native plugin identity is generated by the export macro; remove `id()`",
        ));
    }

    let id = args
        .id
        .map_or_else(|| quote!(env!("CARGO_PKG_NAME")), |id| quote!(#id));
    implementation.items.push(ImplItem::Fn(parse_quote! {
        fn id(&self) -> ::pi_plugin_sdk::PluginId {
            ::pi_plugin_sdk::PluginId::new(__PI_PLUGIN_ID)
        }
    }));

    ensure_async_trait(
        implementation,
        quote!(::pi_plugin_sdk::__plugin_async_trait),
    );

    let self_type = &implementation.self_ty;
    let (kind_value, trait_type, constructor_name, constructor_alias) = match kind {
        PluginKind::Agent => (
            quote!(::pi_plugin_sdk::NativePluginKind::Agent as u32),
            quote!(::pi_plugin_sdk::AgentPlugin),
            Ident::new("pi_agent_plugin_create_v3", proc_macro2::Span::call_site()),
            quote!(::pi_plugin_sdk::AgentPluginCreateV3),
        ),
        PluginKind::Provider => (
            quote!(::pi_plugin_sdk::NativePluginKind::Provider as u32),
            quote!(::pi_plugin_sdk::ProviderPlugin),
            Ident::new(
                "pi_provider_plugin_create_v3",
                proc_macro2::Span::call_site(),
            ),
            quote!(::pi_plugin_sdk::ProviderPluginCreateV3),
        ),
        PluginKind::Session => (
            quote!(::pi_plugin_sdk::NativePluginKind::Session as u32),
            quote!(::pi_plugin_sdk::SessionPlugin),
            Ident::new(
                "pi_session_plugin_create_v3",
                proc_macro2::Span::call_site(),
            ),
            quote!(::pi_plugin_sdk::SessionPluginCreateV3),
        ),
    };

    let construct = if args.factory {
        quote! {
            let options = ::pi_plugin_sdk::decode_plugin_options::<<#self_type as ::pi_plugin_sdk::NativePluginFactory>::Options>(options)?;
            <#self_type as ::pi_plugin_sdk::NativePluginFactory>::load(context, options)
        }
    } else {
        quote! {
            ::pi_plugin_sdk::ensure_empty_plugin_options(options)?;
            Ok(<#self_type as ::core::default::Default>::default())
        }
    };
    let schema = if args.factory {
        quote! {
            ::pi_plugin_sdk::plugin_options_schema::<<#self_type as ::pi_plugin_sdk::NativePluginFactory>::Options>()
        }
    } else {
        quote! {
            ::pi_plugin_sdk::empty_plugin_options_schema()
        }
    };

    Ok(quote! {
        const __PI_PLUGIN_ID: &str = #id;

        #implementation

        static __PI_PLUGIN_DESCRIPTOR_V1: ::pi_plugin_sdk::NativePluginDescriptorV1 =
            ::pi_plugin_sdk::NativePluginDescriptorV1 {
                abi_version: ::pi_plugin_sdk::NATIVE_PLUGIN_ABI_VERSION,
                kind: #kind_value,
                id: ::pi_plugin_sdk::NativeBytes::from_static(__PI_PLUGIN_ID.as_bytes()),
                version: ::pi_plugin_sdk::NativeBytes::from_static(env!("CARGO_PKG_VERSION").as_bytes()),
                build_fingerprint: ::pi_plugin_sdk::NativeBytes::from_static(::pi_plugin_sdk::BUILD_FINGERPRINT.as_bytes()),
            };

        #[unsafe(no_mangle)]
        pub extern "C" fn pi_plugin_descriptor_v1() -> *const ::pi_plugin_sdk::NativePluginDescriptorV1 {
            &__PI_PLUGIN_DESCRIPTOR_V1
        }

        #[unsafe(no_mangle)]
        pub fn #constructor_name(
            context: &::pi_plugin_sdk::PluginLoadContext,
            options: &::pi_plugin_sdk::PluginOptionsValue,
        ) -> ::core::result::Result<::std::sync::Arc<dyn #trait_type>, ::pi_plugin_sdk::PluginLoadError> {
            let construct = ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| {
                #construct
            }));
            match construct {
                Ok(result) => result.map(|plugin| ::std::sync::Arc::new(plugin) as ::std::sync::Arc<dyn #trait_type>),
                Err(_) => Err(::pi_plugin_sdk::PluginLoadError::Initialization(
                    "plugin constructor panicked".to_string(),
                )),
            }
        }

        const _: #constructor_alias = #constructor_name;

        #[unsafe(no_mangle)]
        pub fn pi_plugin_options_schema_v1() -> ::std::string::String {
            #schema
        }
    })
}

fn inject_agent_hook_interests(
    implementation: &mut ItemImpl,
    contract_root: proc_macro2::TokenStream,
) -> syn::Result<()> {
    let Some((_, trait_path, _)) = &implementation.trait_ else {
        return Err(syn::Error::new_spanned(
            implementation,
            "`agent_plugin` must annotate an impl of AgentPlugin",
        ));
    };
    if trait_path
        .segments
        .last()
        .is_none_or(|segment| segment.ident != "AgentPlugin")
    {
        return Err(syn::Error::new_spanned(
            trait_path,
            "`agent_plugin` must annotate an impl of AgentPlugin",
        ));
    }
    if implementation.items.iter().any(
        |item| matches!(item, ImplItem::Fn(function) if function.sig.ident == "hook_interests"),
    ) {
        return Err(syn::Error::new_spanned(
            implementation,
            "agent hook interests are generated from implemented callbacks; remove `hook_interests()`",
        ));
    }

    let hook_variants = implementation.items.iter().filter_map(|item| {
        let ImplItem::Fn(function) = item else {
            return None;
        };
        let variant = match function.sig.ident.to_string().as_str() {
            "input" => quote!(Input),
            "before_agent_start" => quote!(BeforeAgentStart),
            "agent_start" => quote!(AgentStart),
            "agent_end" => quote!(AgentEnd),
            "agent_settled" => quote!(AgentSettled),
            "turn_start" => quote!(TurnStart),
            "turn_end" => quote!(TurnEnd),
            "message_start" => quote!(MessageStart),
            "message_update" => quote!(MessageUpdate),
            "message_end" => quote!(MessageEnd),
            "tool_execution_start" => quote!(ToolExecutionStart),
            "tool_execution_update" => quote!(ToolExecutionUpdate),
            "tool_execution_end" => quote!(ToolExecutionEnd),
            "context" => quote!(Context),
            "tool_call" => quote!(ToolCall),
            "tool_result" => quote!(ToolResult),
            _ => return None,
        };
        Some(variant)
    });
    let hook_paths: Vec<_> = hook_variants
        .map(|variant| quote!(#contract_root::AgentHook::#variant))
        .collect();

    implementation.items.push(ImplItem::Fn(parse_quote! {
        fn hook_interests(&self) -> #contract_root::AgentHookInterests {
            #contract_root::AgentHookInterests::from_hooks(&[
                #(#hook_paths),*
            ])
        }
    }));
    Ok(())
}

fn ensure_async_trait(implementation: &mut ItemImpl, async_trait_path: proc_macro2::TokenStream) {
    let has_async_method = implementation
        .items
        .iter()
        .any(|item| matches!(item, ImplItem::Fn(function) if function.sig.asyncness.is_some()));
    let has_async_trait_attribute = implementation.attrs.iter().any(|attribute| {
        attribute
            .path()
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "async_trait")
    });
    if has_async_method && !has_async_trait_attribute {
        implementation
            .attrs
            .push(parse_quote!(#[#async_trait_path]));
    }
}
