//! `#[atomic2fmu]` — genera el wrapper FMI de un modelo DEVS **atómico** de xdevs.
//!
//! Soporta **N puertos de entrada y M de salida**: la macro inspecciona `type Input` /
//! `type Output` del `impl Component` y se adapta a la forma que sea —
//! `Port<T,N>` (un puerto), `()` (ninguno), una tupla `(Port<A>, Port<B>)`, un array
//! `[Port<T>; K]`, o un **struct de puertos** con `#[derive(Bag)]` (como los de xdevs).
//!
//! Dos formas de invocarla:
//!
//! **1. Sobre un módulo que CONTIENE el modelo** (recomendada). La macro lee el modelo,
//! saca los puertos y, si una salida es un `enum` del módulo, genera una variable one-hot
//! por variante. Solo hace falta el `init` (o que el modelo implemente `Default`).
//!
//! **2. Sobre un struct** con `#[input]`/`#[output]` (cuando el modelo está en otro
//! crate). De momento, un puerto de entrada y uno de salida.
//!
//! En cada compilación vuelca el código generado a `target/atomic2fmu/<Fmu>.rs`
//! (desactivable con `ATOMIC2FMU_NO_DUMP=1`).

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::{
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
    Expr, GenericArgument, Ident, Item, ItemMod, ItemStruct, Meta, MetaNameValue, PathArguments,
    Token, Type, Visibility,
};

// ═════════════════════════════════════════════════════════════════════════════
// Cómo se accede a un puerto dentro de una bolsa (Input/Output)
// ═════════════════════════════════════════════════════════════════════════════

/// Forma de llegar a un puerto concreto desde la bolsa (`inp` / `out`).
#[derive(Clone)]
enum Access {
    Whole,             // la bolsa ES el puerto (Port<T,N>)
    Tuple(syn::Index), // tupla: `.0`, `.1`, ...
    Array(usize),      // array: `[0]`, `[1]`, ...
    Field(Ident),      // struct de puertos: `.nombre`
}

impl Access {
    fn of(&self, base: &TokenStream2) -> TokenStream2 {
        match self {
            Access::Whole => quote! { #base },
            Access::Tuple(i) => quote! { #base.#i },
            Access::Array(i) => {
                let i = *i;
                quote! { #base[#i] }
            }
            Access::Field(f) => quote! { #base.#f },
        }
    }
}

/// Un puerto extraído de `type Input`/`type Output`: cómo se accede, qué dato lleva y un
/// nombre base para las variables FMI.
struct PortInfo {
    access: Access,
    data: Type,
    base_name: String,
}

/// Átomo (`DevsFmu::atomic`, `Simulator`) o acoplado (`DevsFmu::coupled`, `Coordinator`).
#[derive(Clone, Copy, PartialEq)]
enum SimKind {
    Atomic,
    Coupled,
}

impl SimKind {
    /// El tipo del simulador que envuelve al modelo.
    fn sim_ty(&self, model: &Type) -> TokenStream2 {
        match self {
            SimKind::Atomic => quote! { ::xdevs_fmi::Simulator<#model> },
            SimKind::Coupled => quote! { ::xdevs_fmi::Coordinator<#model> },
        }
    }
    /// El constructor de `DevsFmu` (`atomic` / `coupled`).
    fn ctor(&self) -> Ident {
        match self {
            SimKind::Atomic => format_ident!("atomic"),
            SimKind::Coupled => format_ident!("coupled"),
        }
    }
    /// El trait DEVS que el modelo debe implementar.
    fn trait_name(&self) -> &'static str {
        match self {
            SimKind::Atomic => "Atomic",
            SimKind::Coupled => "Coupled",
        }
    }
    /// El `type Kind` que debe declarar el `impl Component` del modelo.
    fn kind_ty_name(&self) -> &'static str {
        match self {
            SimKind::Atomic => "AtomicKind",
            SimKind::Coupled => "CoupledKind",
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Especificación normalizada (de aquí sale el código)
// ═════════════════════════════════════════════════════════════════════════════

struct InPort {
    field: Ident,   // nombre de la variable FMI de entrada
    access: Access, // dónde va en la bolsa de entrada
    ty: Type,       // tipo FMI (= tipo del puerto)
    start: Expr,
}

/// Una variable FMI de salida derivada de un puerto.
struct OutVar {
    field: Ident,
    from: Option<Expr>, // conversión; None = identidad (self.field = ev)
    ty: Type,
    start: Expr,
}

struct OutPort {
    access: Access,     // dónde leer en la bolsa de salida
    vars: Vec<OutVar>,  // 1 (primitivo) o N (enum one-hot) variables
}

struct SpecTaOut {
    field: Ident,
    ty: Type,
    start: Expr,
}

struct Spec {
    name: Ident,
    vis: Visibility,
    kind: SimKind,
    model_init: Expr,
    model_ty: Type,
    in_ports: Vec<InPort>,
    out_ports: Vec<OutPort>,
    ta_outputs: Vec<SpecTaOut>,
}

/// Genera el struct FMU + Default + UserModel + export_fmu! a partir de la Spec.
fn gen(spec: &Spec) -> TokenStream2 {
    let Spec {
        name,
        vis,
        kind,
        model_init,
        model_ty,
        in_ports,
        out_ports,
        ta_outputs,
    } = spec;
    let vref = format_ident!("{}ValueRef", name);
    let sim_ty = kind.sim_ty(model_ty);
    let ctor = kind.ctor();

    let mut struct_fields = Vec::new();
    let mut default_fields = Vec::new();

    struct_fields.push(quote! {
        #[variable(causality = Input, interval_variability = Countdown)]
        ta: ::fmi_export::fmi3::Clock
    });
    default_fields.push(quote! { ta: ::core::default::Default::default() });

    // Relojes triggered + variable clocked de cada puerto de entrada.
    let mut input_ev_idents = Vec::new();
    for p in in_ports {
        let ev = format_ident!("{}_ev", p.field);
        let data = &p.field;
        let ty = &p.ty;
        let start = &p.start;
        struct_fields.push(quote! {
            #[variable(causality = Input, interval_variability = Triggered)]
            #ev: ::fmi_export::fmi3::Clock
        });
        struct_fields.push(quote! {
            #[variable(causality = Input, variability = Discrete, start = #start, clocks = [#ev])]
            #data: #ty
        });
        default_fields.push(quote! { #ev: ::core::default::Default::default() });
        default_fields.push(quote! { #data: #start });
        input_ev_idents.push(ev);
    }

    // Variables de cada puerto de salida (clockeadas por [ta]).
    for port in out_ports {
        for v in &port.vars {
            let data = &v.field;
            let ty = &v.ty;
            let start = &v.start;
            struct_fields.push(quote! {
                #[variable(causality = Output, variability = Discrete, start = #start, clocks = [ta])]
                #data: #ty
            });
            default_fields.push(quote! { #data: #start });
        }
    }
    // Salidas de tipo `ta` (σ restante): clockeadas por [ta, todos los _ev].
    for o in ta_outputs {
        let data = &o.field;
        let ty = &o.ty;
        let start = &o.start;
        let clocks = if input_ev_idents.is_empty() {
            quote! { [ta] }
        } else {
            quote! { [ta, #(#input_ev_idents),*] }
        };
        struct_fields.push(quote! {
            #[variable(causality = Output, variability = Discrete, start = #start, clocks = #clocks)]
            #data: #ty
        });
        default_fields.push(quote! { #data: #start });
    }

    struct_fields.push(quote! {
        sim: ::xdevs_fmi::DevsFmu<#sim_ty>
    });
    default_fields.push(quote! { sim: ::xdevs_fmi::DevsFmu::#ctor(#model_init) });

    // ── Bloque de captura de salidas: lee cada puerto de salida y aplica sus vars ──
    let capture = |fill: TokenStream2| -> TokenStream2 {
        if out_ports.is_empty() {
            return quote! { self.sim.step(time, #fill, |_out| {}); };
        }
        let mut locals = Vec::new();
        let mut reads = Vec::new();
        let mut applies = Vec::new();
        for (k, port) in out_ports.iter().enumerate() {
            let ev = format_ident!("__ev_{}", k);
            locals.push(quote! { let mut #ev = ::core::option::Option::None; });
            let read = port.access.of(&quote! { __out });
            reads.push(quote! { #ev = #read.get_values().last().copied(); });
            let sets = port.vars.iter().map(|v| {
                let f = &v.field;
                match &v.from {
                    Some(expr) => quote! { self.#f = (#expr)(__ev); },
                    None => quote! { self.#f = __ev; },
                }
            });
            applies.push(quote! {
                if let ::core::option::Option::Some(__ev) = #ev { #(#sets)* }
            });
        }
        quote! {
            #(#locals)*
            self.sim.step(time, #fill, |__out| { #(#reads)* });
            #(#applies)*
        }
    };

    let ta_arm = {
        let body = capture(quote! { |_inp| {} });
        quote! { ::core::result::Result::Ok(#vref::Ta) => { #body } }
    };
    let input_arms: Vec<TokenStream2> = in_ports
        .iter()
        .map(|p| {
            let data = &p.field;
            let ev = format_ident!("{}_ev", p.field);
            let variant = format_ident!("{}", pascal(&ev.to_string()));
            let dst = p.access.of(&quote! { __inp });
            let fill = quote! { |__inp| { let _ = #dst.add_value(__v); } };
            let body = capture(fill);
            quote! {
                ::core::result::Result::Ok(#vref::#variant) => {
                    let __v = self.#data;
                    #body
                }
            }
        })
        .collect();

    let ta_refresh: Vec<TokenStream2> = ta_outputs
        .iter()
        .map(|o| {
            let data = &o.field;
            quote! { self.#data = self.sim.ta(); }
        })
        .collect();

    quote! {
        #[derive(::fmi_export::FmuModel)]
        #[model(
            model_exchange = false,
            co_simulation = false,
            scheduled_execution = true,
            user_model = false,
            vr_enum = true
        )]
        #vis struct #name {
            #(#struct_fields,)*
        }

        impl ::core::default::Default for #name {
            fn default() -> Self {
                Self { #(#default_fields,)* }
            }
        }

        impl ::fmi_export::fmi3::UserModel for #name {
            type LoggingCategory = ::fmi_export::fmi3::DefaultLoggingCategory;

            fn calculate_values(
                &mut self,
                _context: &dyn ::fmi_export::fmi3::Context<Self>,
            ) -> ::core::result::Result<::fmi::fmi3::Fmi3Res, ::fmi::fmi3::Fmi3Error> {
                #(#ta_refresh)*
                ::core::result::Result::Ok(::fmi::fmi3::Fmi3Res::OK)
            }

            fn activate_partition(
                &mut self,
                _context: &mut dyn ::fmi_export::fmi3::Context<Self>,
                clock_reference: ::fmi::fmi3::binding::fmi3ValueReference,
                time: f64,
            ) -> ::core::result::Result<::fmi::fmi3::Fmi3Res, ::fmi::fmi3::Fmi3Error> {
                match #vref::try_from(clock_reference) {
                    #ta_arm
                    #(#input_arms)*
                    _ => return ::core::result::Result::Err(::fmi::fmi3::Fmi3Error::Error),
                }
                ::core::result::Result::Ok(::fmi::fmi3::Fmi3Res::OK)
            }

            fn next_interval(
                &self,
                clock_reference: ::fmi::fmi3::binding::fmi3ValueReference,
            ) -> ::core::option::Option<f64> {
                match #vref::try_from(clock_reference) {
                    ::core::result::Result::Ok(#vref::Ta) => {
                        ::core::option::Option::Some(self.sim.ta())
                    }
                    _ => ::core::option::Option::None,
                }
            }
        }

        ::fmi_export::export_fmu!(#name);
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Argumentos de la macro
// ═════════════════════════════════════════════════════════════════════════════

struct MacroArgs {
    model: Option<syn::Path>,
    init: Option<Expr>,
}

impl Parse for MacroArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let pairs = Punctuated::<MetaNameValue, Token![,]>::parse_terminated(input)?;
        let mut model = None;
        let mut init = None;
        for p in pairs {
            let key = p.path.get_ident().map(Ident::to_string).unwrap_or_default();
            match key.as_str() {
                "model" => match &p.value {
                    Expr::Path(ep) => model = Some(ep.path.clone()),
                    other => return Err(syn::Error::new_spanned(other, "`model` debe ser un tipo")),
                },
                "init" => init = Some(p.value.clone()),
                other => {
                    return Err(syn::Error::new_spanned(
                        &p.path,
                        format!("clave desconocida `{other}` (se esperan `model`, `init`)"),
                    ))
                }
            }
        }
        Ok(MacroArgs { model, init })
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Punto de entrada: detecta si es un módulo o un struct
// ═════════════════════════════════════════════════════════════════════════════

/// Genera el wrapper FMI de un modelo DEVS **atómico** (`impl xdevs::Atomic`).
#[proc_macro_attribute]
pub fn atomic2fmu(args: TokenStream, item: TokenStream) -> TokenStream {
    dispatch(args, item, SimKind::Atomic)
}

/// Genera el wrapper FMI de un modelo DEVS **acoplado** (`impl xdevs::Coupled`,
/// normalmente con `#[xdevs::coupled]`). Igual que `atomic2fmu` pero conduce el modelo
/// con un `Coordinator` (`DevsFmu::coupled`); los puertos externos (Input/Output del
/// acoplado) se exponen igual que en un átomo.
#[proc_macro_attribute]
pub fn coupled2fmu(args: TokenStream, item: TokenStream) -> TokenStream {
    dispatch(args, item, SimKind::Coupled)
}

fn dispatch(args: TokenStream, item: TokenStream, kind: SimKind) -> TokenStream {
    let args = syn::parse_macro_input!(args as MacroArgs);
    if let Ok(item_mod) = syn::parse::<ItemMod>(item.clone()) {
        return match expand_mod(args, item_mod, kind) {
            Ok(ts) => ts.into(),
            Err(e) => e.to_compile_error().into(),
        };
    }
    let item_struct = syn::parse_macro_input!(item as ItemStruct);
    match expand_struct(args, item_struct, kind) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Forma MÓDULO: lee el modelo del propio módulo (N puertos)
// ═════════════════════════════════════════════════════════════════════════════

fn expand_mod(args: MacroArgs, mut item: ItemMod, kind: SimKind) -> syn::Result<TokenStream2> {
    let Some((_brace, items)) = &mut item.content else {
        return Err(syn::Error::new_spanned(
            &item,
            "el módulo debe tener cuerpo `{ ... }` con el modelo DEVS dentro",
        ));
    };

    let (model_ty, input_ty, output_ty) = find_component(items, kind).ok_or_else(|| {
        let k = kind.kind_ty_name();
        syn::Error::new_spanned(
            &item.ident,
            format!(
                "no se encontró `impl xdevs::Component for <Modelo>` con `type Kind = xdevs::{k}` \
                 dentro del módulo (el modelo DEVS entero debe estar DENTRO del `mod`).",
            ),
        )
    })?;

    // El modelo debe implementar el trait DEVS del tipo (Atomic o Coupled).
    let want = kind.trait_name();
    if !has_impl_named(items, want) {
        let (macro_name, other) = match kind {
            SimKind::Atomic => ("atomic2fmu", "un acoplado (`impl Coupled`) pediría `coupled2fmu`"),
            SimKind::Coupled => ("coupled2fmu", "un átomo (`impl Atomic`) usa `atomic2fmu`"),
        };
        return Err(syn::Error::new_spanned(
            &item.ident,
            format!("#[{macro_name}]: el modelo debe implementar `xdevs::{want}` — {other}."),
        ));
    }

    let model_path = match &model_ty {
        Type::Path(p) => p.path.clone(),
        _ => return Err(syn::Error::new_spanned(&model_ty, "el modelo debe ser un tipo con nombre")),
    };
    let model_ident = model_path
        .segments
        .last()
        .map(|s| s.ident.clone())
        .ok_or_else(|| syn::Error::new_spanned(&model_ty, "modelo sin nombre"))?;

    // Extraer los puertos de entrada y salida de sus tipos.
    let in_shapes = extract_ports(&input_ty, items, "input")?;
    let out_shapes = extract_ports(&output_ty, items, "output")?;

    let init = args
        .init
        .unwrap_or_else(|| syn::parse_quote!( <#model_path as ::core::default::Default>::default() ));

    // Entradas: cada puerto → una variable FMI del tipo del puerto (identidad).
    let in_ports = in_shapes
        .into_iter()
        .map(|p| InPort {
            field: format_ident!("{}", p.base_name),
            access: p.access,
            start: default_start(&p.data),
            ty: p.data,
        })
        .collect();

    // Salidas: enum del módulo → one-hot; primitivo → una variable identidad.
    let f64_ty: Type = syn::parse_quote!(f64);
    let single_out = out_shapes.len() == 1;
    let out_ports = out_shapes
        .into_iter()
        .map(|p| {
            match find_enum_variants(items, &p.data) {
                Some(variants) => {
                    let data = p.data.clone();
                    let vars = variants
                        .iter()
                        .map(|v| {
                            // nombre: si hay UN solo puerto de salida → variante a secas
                            // (green/red); si hay varios → prefijado con el puerto.
                            let field = if single_out {
                                format_ident!("{}", to_snake(&v.to_string()))
                            } else {
                                format_ident!("{}_{}", p.base_name, to_snake(&v.to_string()))
                            };
                            let from: Expr = syn::parse_quote!(
                                |__s: #data| if __s == #data::#v { 1.0 } else { 0.0 }
                            );
                            OutVar {
                                field,
                                from: Some(from),
                                ty: f64_ty.clone(),
                                start: default_start(&f64_ty),
                            }
                        })
                        .collect();
                    OutPort { access: p.access, vars }
                }
                None => OutPort {
                    access: p.access,
                    vars: vec![OutVar {
                        field: format_ident!("{}", p.base_name),
                        from: None,
                        start: default_start(&p.data),
                        ty: p.data,
                    }],
                },
            }
        })
        .collect();

    let spec = Spec {
        name: format_ident!("{}Fmu", model_ident),
        vis: syn::parse_quote!(pub),
        kind,
        model_init: init,
        model_ty,
        in_ports,
        out_ports,
        ta_outputs: Vec::new(),
    };

    let wrapper = gen(&spec);
    dump_generado(&spec.name, &wrapper);
    let wrapper_items: syn::File = syn::parse2(wrapper)?;
    items.extend(wrapper_items.items);

    Ok(quote! { #item })
}

/// Extrae los puertos de un tipo de bolsa (`Input`/`Output`).
/// Soporta: `()`, `Port<T,N>`, tupla, array `[Port<T>; K]` y struct de puertos.
fn extract_ports(ty: &Type, items: &[Item], prefix: &str) -> syn::Result<Vec<PortInfo>> {
    // 1) Puerto único `Port<T,N>`.
    if let Some(data) = port_data(ty) {
        return Ok(vec![PortInfo {
            access: Access::Whole,
            data,
            base_name: prefix.to_string(),
        }]);
    }
    match ty {
        // 2) Sin puertos: `()`.
        Type::Tuple(t) if t.elems.is_empty() => Ok(Vec::new()),
        // 3) Tupla de puertos `(Port<A>, Port<B>, ...)`.
        Type::Tuple(t) => {
            let mut ports = Vec::new();
            for (i, elem) in t.elems.iter().enumerate() {
                let data = port_data(elem).ok_or_else(|| {
                    syn::Error::new_spanned(elem, "cada elemento de la tupla debe ser un `Port<...>`")
                })?;
                ports.push(PortInfo {
                    access: Access::Tuple(syn::Index::from(i)),
                    data,
                    base_name: format!("{prefix}_{i}"),
                });
            }
            Ok(ports)
        }
        // 4) Array de puertos `[Port<T>; K]`.
        Type::Array(arr) => {
            let data = port_data(&arr.elem).ok_or_else(|| {
                syn::Error::new_spanned(&arr.elem, "el elemento del array debe ser un `Port<...>`")
            })?;
            let k = match &arr.len {
                Expr::Lit(syn::ExprLit { lit: syn::Lit::Int(n), .. }) => {
                    n.base10_parse::<usize>()?
                }
                _ => {
                    return Err(syn::Error::new_spanned(
                        &arr.len,
                        "la longitud del array debe ser un literal entero",
                    ))
                }
            };
            Ok((0..k)
                .map(|i| PortInfo {
                    access: Access::Array(i),
                    data: data.clone(),
                    base_name: format!("{prefix}_{i}"),
                })
                .collect())
        }
        // 5) Struct de puertos: buscar la definición en el módulo.
        Type::Path(p) => {
            let name = p
                .path
                .segments
                .last()
                .map(|s| s.ident.clone())
                .ok_or_else(|| syn::Error::new_spanned(p, "tipo de bolsa sin nombre"))?;
            let strct = items.iter().find_map(|it| match it {
                Item::Struct(s) if s.ident == name => Some(s),
                _ => None,
            });
            let Some(strct) = strct else {
                return Err(syn::Error::new_spanned(
                    p,
                    format!(
                        "no se pudo interpretar la bolsa `{name}`: no es `Port<...>`, ni tupla, ni \
                         array, ni un struct de puertos definido dentro del módulo",
                    ),
                ));
            };
            let mut ports = Vec::new();
            for field in &strct.fields {
                let fname = field
                    .ident
                    .clone()
                    .ok_or_else(|| syn::Error::new_spanned(field, "el struct de puertos debe tener campos con nombre"))?;
                let data = port_data(&field.ty).ok_or_else(|| {
                    syn::Error::new_spanned(&field.ty, "cada campo del struct de puertos debe ser un `Port<...>`")
                })?;
                ports.push(PortInfo {
                    access: Access::Field(fname.clone()),
                    data,
                    base_name: fname.to_string(),
                });
            }
            Ok(ports)
        }
        _ => Err(syn::Error::new_spanned(
            ty,
            "tipo de bolsa no soportado (usa `Port<T,N>`, `()`, una tupla, un array o un struct de puertos)",
        )),
    }
}

/// Busca el `impl ... Component for T` cuyo `type Kind` coincide con `kind` (para que en
/// un módulo con varios componentes se elija el modelo correcto: el acoplado para
/// `coupled2fmu`, el átomo para `atomic2fmu`). Devuelve (T, Input, Output).
fn find_component(items: &[Item], kind: SimKind) -> Option<(Type, Type, Type)> {
    let want = kind.kind_ty_name();
    for item in items {
        let Item::Impl(imp) = item else { continue };
        let Some((_, trait_path, _)) = &imp.trait_ else { continue };
        let is_component = trait_path
            .segments
            .last()
            .map(|s| s.ident == "Component")
            .unwrap_or(false);
        if !is_component {
            continue;
        }
        let mut input = None;
        let mut output = None;
        let mut kind_ty = None;
        for ii in &imp.items {
            if let syn::ImplItem::Type(t) = ii {
                match () {
                    _ if t.ident == "Input" => input = Some(t.ty.clone()),
                    _ if t.ident == "Output" => output = Some(t.ty.clone()),
                    _ if t.ident == "Kind" => kind_ty = Some(t.ty.clone()),
                    _ => {}
                }
            }
        }
        // Debe declarar el Kind esperado (AtomicKind / CoupledKind).
        let matches_kind = matches!(&kind_ty, Some(Type::Path(p))
            if p.path.segments.last().map(|s| s.ident == want).unwrap_or(false));
        if matches_kind {
            return Some(((*imp.self_ty).clone(), input?, output?));
        }
    }
    None
}

/// ¿Hay algún `impl <Trait> for ...` cuyo trait termine en `name`?
fn has_impl_named(items: &[Item], name: &str) -> bool {
    items.iter().any(|it| {
        if let Item::Impl(imp) = it {
            if let Some((_, tp, _)) = &imp.trait_ {
                return tp.segments.last().map(|s| s.ident == name).unwrap_or(false);
            }
        }
        false
    })
}

/// De `Port<T, N>` saca `T`; de `()` devuelve `None`.
fn port_data(ty: &Type) -> Option<Type> {
    if let Type::Path(p) = ty {
        let seg = p.path.segments.last()?;
        if seg.ident == "Port" {
            if let PathArguments::AngleBracketed(args) = &seg.arguments {
                for a in &args.args {
                    if let GenericArgument::Type(t) = a {
                        return Some(t.clone());
                    }
                }
            }
        }
    }
    None
}

/// Si `data_ty` es un `enum` definido en el módulo, devuelve sus variantes.
fn find_enum_variants(items: &[Item], data_ty: &Type) -> Option<Vec<Ident>> {
    let name = match data_ty {
        Type::Path(p) => p.path.segments.last()?.ident.clone(),
        _ => return None,
    };
    for item in items {
        if let Item::Enum(e) = item {
            if e.ident == name {
                return Some(e.variants.iter().map(|v| v.ident.clone()).collect());
            }
        }
    }
    None
}

// ═════════════════════════════════════════════════════════════════════════════
// Forma STRUCT: la interfaz la declara el usuario con #[input]/#[output]
// (de momento: un puerto de entrada y uno de salida)
// ═════════════════════════════════════════════════════════════════════════════

fn expand_struct(args: MacroArgs, item: ItemStruct, kind: SimKind) -> syn::Result<TokenStream2> {
    let model_path = args
        .model
        .ok_or_else(|| syn::Error::new(Span::call_site(), "falta `model = <Tipo>` (forma struct)"))?;
    let init = args
        .init
        .ok_or_else(|| syn::Error::new(Span::call_site(), "falta `init = <expr>` (forma struct)"))?;
    let model_ty: Type = syn::parse_quote!(#model_path);

    let mut in_ports = Vec::new();
    let mut out_vars = Vec::new();
    let mut ta_outputs = Vec::new();

    for field in &item.fields {
        let fname = field
            .ident
            .clone()
            .ok_or_else(|| syn::Error::new_spanned(field, "se esperan campos con nombre"))?;
        let ty = field.ty.clone();
        let mut handled = false;

        for attr in &field.attrs {
            if attr.path().is_ident("input") {
                let mut start = None;
                if let Meta::List(_) = &attr.meta {
                    let metas = attr.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)?;
                    for m in &metas {
                        if let Meta::NameValue(nv) = m {
                            if nv.path.is_ident("start") {
                                start = Some(nv.value.clone());
                            }
                        }
                    }
                }
                let start = start.unwrap_or_else(|| default_start(&ty));
                in_ports.push(InPort {
                    field: fname.clone(),
                    access: Access::Whole,
                    ty: ty.clone(),
                    start,
                });
                handled = true;
            } else if attr.path().is_ident("output") {
                let (from, ta, start) = match &attr.meta {
                    Meta::List(_) => {
                        let metas = attr.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)?;
                        parse_output_args(&metas)?
                    }
                    _ => (None, false, None),
                };
                let start = start.unwrap_or_else(|| default_start(&ty));
                if ta {
                    ta_outputs.push(SpecTaOut { field: fname.clone(), ty: ty.clone(), start });
                } else {
                    out_vars.push(OutVar { field: fname.clone(), from, ty: ty.clone(), start });
                }
                handled = true;
            }
        }
        if !handled {
            return Err(syn::Error::new_spanned(
                field,
                "cada campo debe llevar `#[input]` o `#[output(...)]`",
            ));
        }
    }

    // Forma struct: todas las salidas `from` salen del único puerto de salida (Whole).
    let out_ports = if out_vars.is_empty() {
        Vec::new()
    } else {
        vec![OutPort { access: Access::Whole, vars: out_vars }]
    };

    let spec = Spec {
        name: item.ident.clone(),
        vis: item.vis.clone(),
        kind,
        model_init: init,
        model_ty,
        in_ports,
        out_ports,
        ta_outputs,
    };
    let out = gen(&spec);
    dump_generado(&spec.name, &out);
    Ok(out)
}

fn parse_output_args(
    metas: &Punctuated<Meta, Token![,]>,
) -> syn::Result<(Option<Expr>, bool, Option<Expr>)> {
    let mut from = None;
    let mut ta = false;
    let mut start = None;
    for m in metas {
        match m {
            Meta::Path(p) if p.is_ident("ta") => ta = true,
            Meta::NameValue(nv) if nv.path.is_ident("from") => from = Some(nv.value.clone()),
            Meta::NameValue(nv) if nv.path.is_ident("start") => start = Some(nv.value.clone()),
            other => {
                return Err(syn::Error::new_spanned(
                    other,
                    "en `#[output(...)]` se esperan `from = |ev| ...`, `ta` o `start = ...`",
                ))
            }
        }
    }
    Ok((from, ta, start))
}

// ═════════════════════════════════════════════════════════════════════════════
// Utilidades
// ═════════════════════════════════════════════════════════════════════════════

/// Vuelca el código generado a `<crate>/target/atomic2fmu/<Nombre>.rs` en cada
/// compilación, para seguir el paso modelo DEVS → FMI. No fatal; se desactiva con
/// `ATOMIC2FMU_NO_DUMP`.
fn dump_generado(name: &Ident, tokens: &TokenStream2) {
    if std::env::var_os("ATOMIC2FMU_NO_DUMP").is_some() {
        return;
    }
    let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") else {
        return;
    };
    let dir = std::path::Path::new(&manifest).join("target").join("atomic2fmu");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let pretty = syn::parse2::<syn::File>(tokens.clone())
        .map(|f| prettyplease::unparse(&f))
        .unwrap_or_else(|_| tokens.to_string());
    let header = format!(
        "// GENERADO por #[atomic2fmu] para `{name}` — NO editar (se regenera al compilar).\n\
         // Este es el \"código FMI\" que la macro produce a partir del modelo DEVS,\n\
         // el paso intermedio antes de `cargo fmi bundle`.\n\n"
    );
    let _ = std::fs::write(dir.join(format!("{name}.rs")), header + &pretty);
}

fn default_start(ty: &Type) -> Expr {
    let ident = match ty {
        Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
        _ => None,
    };
    let lit = match ident.as_deref() {
        Some("bool") => quote! { false },
        Some("i8") | Some("i16") | Some("i32") | Some("i64") | Some("isize") | Some("u8")
        | Some("u16") | Some("u32") | Some("u64") | Some("usize") => quote! { 0 },
        _ => quote! { 0.0 },
    };
    syn::parse2(lit).expect("literal de start por defecto válido")
}

fn pascal(name: &str) -> String {
    name.split('_')
        .filter(|s| !s.is_empty())
        .map(|s| {
            let mut c = s.chars();
            c.next()
                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
        })
        .collect()
}

fn to_snake(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.char_indices() {
        if c.is_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}
