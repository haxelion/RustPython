use crate::error::Diagnostic;
use crate::util::{
    AttributeExt, ClassItemMeta, ContentItem, ContentItemInner, ItemMeta, ItemNursery,
    SimpleItemMeta, ALL_ALLOWED_NAMES,
};
use proc_macro2::TokenStream;
use quote::{quote, quote_spanned, ToTokens};
use syn::{parse_quote, spanned::Spanned, Attribute, AttributeArgs, Ident, Item, Result, UseTree};
use syn_ext::ext::*;

struct Module {
    name: String,
    module_extend_items: ItemNursery,
}

pub fn impl_pymodule(
    attr: AttributeArgs,
    module_item: Item,
) -> std::result::Result<TokenStream, Diagnostic> {
    let mut module_item = match module_item {
        Item::Mod(m) => m,
        other => bail_span!(other, "#[pymodule] can only be on a module declaration"),
    };
    let fake_ident = Ident::new("pymodule", module_item.span());
    let module_meta =
        SimpleItemMeta::from_nested(module_item.ident.clone(), fake_ident, attr.into_iter())?;
    let mut module_context = Module {
        name: module_meta.simple_name()?,
        module_extend_items: ItemNursery::default(),
    };
    let items = module_item.unbraced_content_mut()?;

    let debug_attrs: Vec<Attribute> = vec![parse_quote!(#[RustPython derive bug!])];
    for item in items.iter_mut() {
        let mut attrs = if let Ok(attrs) = item.attrs_mut() {
            std::mem::replace(attrs, debug_attrs.clone())
        } else {
            continue;
        };
        let mut gen_module_item = || -> Result<()> {
            let (pyitems, cfgs) = attrs_to_pyitems(&attrs, new_item)?;
            for pyitem in pyitems.iter().rev() {
                pyitem.gen_module_item(ModuleItemArgs {
                    item,
                    attrs: &mut attrs,
                    module: &mut module_context,
                    cfgs: cfgs.as_slice(),
                })?;
            }
            Ok(())
        };
        let result = gen_module_item();
        let _ = std::mem::replace(item.attrs_mut().unwrap(), attrs);
        result?
    }

    let module_name = module_context.name.as_str();
    let module_extend_items = module_context.module_extend_items;
    items.extend(vec![
        parse_quote! {
            pub(crate) const MODULE_NAME: &'static str = #module_name;
        },
        parse_quote! {
            pub(crate) fn extend_module(
                vm: &::rustpython_vm::vm::VirtualMachine,
                module: &::rustpython_vm::pyobject::PyObjectRef,
            ) {
                #module_extend_items
            }
        },
        parse_quote! {
            #[allow(dead_code)]
            pub(crate) fn make_module(
                vm: &::rustpython_vm::vm::VirtualMachine
            ) -> ::rustpython_vm::pyobject::PyObjectRef {
                let module = vm.new_module(MODULE_NAME, vm.ctx.new_dict());
                extend_module(vm, &module);
                module
            }
        },
    ]);

    Ok(module_item.into_token_stream())
}

fn new_item(index: usize, attr_name: String, pyattrs: Option<Vec<usize>>) -> Box<dyn ModuleItem> {
    assert!(ALL_ALLOWED_NAMES.contains(&attr_name.as_str()));
    match attr_name.as_str() {
        "pyfunction" => Box::new(FunctionItem {
            inner: ContentItemInner { index, attr_name },
        }),
        "pyattr" => Box::new(AttributeItem {
            inner: ContentItemInner { index, attr_name },
        }),
        "pystruct_sequence" | "pyclass" => Box::new(ClassItem {
            inner: ContentItemInner { index, attr_name },
            pyattrs: pyattrs.unwrap_or_else(Vec::new),
        }),
        other => unreachable!("#[pymodule] doesn't accept #[{}]", other),
    }
}

fn attrs_to_pyitems<F, R>(attrs: &[Attribute], new_item: F) -> Result<(Vec<R>, Vec<Attribute>)>
where
    F: Fn(usize, String, Option<Vec<usize>>) -> R,
{
    let mut cfgs: Vec<Attribute> = Vec::new();
    let mut result = Vec::new();

    let mut iter = attrs.iter().enumerate().peekable();
    while let Some((_, attr)) = iter.peek() {
        // take all cfgs but no py items
        let attr = *attr;
        let attr_name = if let Some(ident) = attr.get_ident() {
            ident.to_string()
        } else {
            continue;
        };
        if attr_name == "cfg" {
            cfgs.push(attr.clone());
        } else if ALL_ALLOWED_NAMES.contains(&attr_name.as_str()) {
            break;
        }
        iter.next();
    }

    let mut closed = false;
    let mut pyattrs = Vec::new();
    for (i, attr) in iter {
        // take py items but no cfgs
        let attr_name = if let Some(ident) = attr.get_ident() {
            ident.to_string()
        } else {
            continue;
        };
        if attr_name == "cfg" {
            return Err(syn::Error::new_spanned(
                attr,
                "#[py*] items must be placed under `cfgs`",
            ));
        }
        if !ALL_ALLOWED_NAMES.contains(&attr_name.as_str()) {
            continue;
        } else if closed {
            return Err(syn::Error::new_spanned(
                attr,
                "Only one #[pyattr] annotated #[py*] item can exist",
            ));
        }

        if attr_name == "pyattr" {
            if !result.is_empty() {
                return Err(syn::Error::new_spanned(
                    attr,
                    "#[pyattr] must be placed on top of other #[py*] items",
                ));
            }
            pyattrs.push((i, attr_name));
            continue;
        }

        if pyattrs.is_empty() {
            result.push(new_item(i, attr_name, None));
        } else {
            if !["pyclass", "pystruct_sequence"].contains(&attr_name.as_str()) {
                return Err(syn::Error::new_spanned(
                    attr,
                    "#[pyattr] #[pyclass] is the only supported composition",
                ));
            }
            let pyattr_indexes = pyattrs.iter().map(|(i, _)| i).copied().collect();
            result.push(new_item(i, attr_name, Some(pyattr_indexes)));
            pyattrs = Vec::new();
            closed = true;
        }
    }
    for (index, attr_name) in pyattrs {
        assert!(!closed);
        result.push(new_item(index, attr_name, None));
    }
    Ok((result, cfgs))
}

/// #[pyfunction]
struct FunctionItem {
    inner: ContentItemInner,
}

/// #[pyclass] or #[pystruct_sequence]
struct ClassItem {
    inner: ContentItemInner,
    pyattrs: Vec<usize>,
}

/// #[pyattr]
struct AttributeItem {
    inner: ContentItemInner,
}

impl ContentItem for FunctionItem {
    fn inner(&self) -> &ContentItemInner {
        &self.inner
    }
}

impl ContentItem for ClassItem {
    fn inner(&self) -> &ContentItemInner {
        &self.inner
    }
}

impl ContentItem for AttributeItem {
    fn inner(&self) -> &ContentItemInner {
        &self.inner
    }
}

struct ModuleItemArgs<'a> {
    item: &'a Item,
    attrs: &'a mut Vec<Attribute>,
    module: &'a mut Module,
    cfgs: &'a [Attribute],
}

impl<'a> ModuleItemArgs<'a> {
    fn module_name(&'a self) -> &'a str {
        self.module.name.as_str()
    }
}

trait ModuleItem: ContentItem {
    fn gen_module_item(&self, args: ModuleItemArgs<'_>) -> Result<()>;
}

impl ModuleItem for FunctionItem {
    fn gen_module_item(&self, args: ModuleItemArgs<'_>) -> Result<()> {
        let ident = match args.item {
            Item::Fn(syn::ItemFn { sig, .. }) => sig.ident.clone(),
            other => return Err(self.new_syn_error(other.span(), "can only be on a function")),
        };

        let item_attr = args.attrs.remove(self.index());
        let item_meta = SimpleItemMeta::from_nested(
            ident.clone(),
            item_attr.get_ident().unwrap().clone(),
            item_attr.promoted_nested()?.into_iter(),
        )?;

        let py_name = item_meta.simple_name()?;
        let item = {
            let module = args.module_name();
            let new_func = quote_spanned!(
                ident.span() => vm.ctx.new_function_named(#ident, #module.to_owned(), #py_name.to_owned())
            );
            quote! {
                vm.__module_set_attr(&module, #py_name, #new_func).unwrap();
            }
        };

        args.module
            .module_extend_items
            .add_item(py_name, args.cfgs.to_vec(), item)?;
        Ok(())
    }
}

impl ModuleItem for ClassItem {
    fn gen_module_item(&self, args: ModuleItemArgs<'_>) -> Result<()> {
        let ident = match args.item {
            Item::Struct(syn::ItemStruct { ident, .. }) => ident.clone(),
            Item::Enum(syn::ItemEnum { ident, .. }) => ident.clone(),
            other => return Err(self.new_syn_error(other.span(), "can only be on a function")),
        };
        let (module_name, class_name) = {
            let class_attr = &mut args.attrs[self.inner.index];
            if self.pyattrs.is_empty() {
                // check noattr before ClassItemMeta::from_nested
                let noattr = class_attr.try_remove_name("noattr")?;
                if noattr.is_none() {
                    return Err(syn::Error::new_spanned(
                        class_attr,
                        format!(
                            "#[{name}] requires #[pyattr] to be a module attribute. \
                         To keep it free type, try #[{name}(noattr)]",
                            name = self.attr_name()
                        ),
                    ));
                }
            }

            let class_meta = ClassItemMeta::from_nested(
                ident.clone(),
                class_attr.get_ident().unwrap().clone(),
                class_attr.promoted_nested()?.into_iter(),
            )?;
            let module_name = args.module.name.clone();
            class_attr.fill_nested_meta("module", || {
                parse_quote! {module = #module_name}
            })?;
            let class_name = class_meta.class_name()?;
            (module_name, class_name)
        };
        for attr_index in self.pyattrs.iter().rev() {
            let attr_attr = args.attrs.remove(*attr_index);
            let (meta_ident, nested) = attr_attr.ident_and_promoted_nested()?;
            let item_meta =
                SimpleItemMeta::from_nested(ident.clone(), meta_ident.clone(), nested.into_iter())?;

            let py_name = item_meta
                .optional_name()
                .unwrap_or_else(|| class_name.clone());
            let new_class = quote_spanned!(ident.span() =>
                #ident::make_class(&vm.ctx);
            );
            let item = quote! {
                let new_class = #new_class;
                new_class.set_str_attr("__module__", vm.ctx.new_str(#module_name));
                vm.__module_set_attr(&module, #py_name, new_class).unwrap();
            };

            args.module
                .module_extend_items
                .add_item(py_name.clone(), args.cfgs.to_vec(), item)?;
        }
        Ok(())
    }
}

impl ModuleItem for AttributeItem {
    fn gen_module_item(&self, args: ModuleItemArgs<'_>) -> Result<()> {
        let get_py_name = |attrs: &mut Vec<Attribute>, ident: &Ident| -> Result<_> {
            let (meta_ident, nested) = attrs[self.inner.index].ident_and_promoted_nested()?;
            let item_meta =
                SimpleItemMeta::from_nested(ident.clone(), meta_ident.clone(), nested.into_iter())?;
            let py_name = item_meta.simple_name()?;
            Ok(py_name)
        };
        let (py_name, tokens) = match args.item {
            Item::Fn(syn::ItemFn { sig, .. }) => {
                let ident = &sig.ident;
                let py_name = get_py_name(args.attrs, &ident)?;
                (
                    py_name.clone(),
                    quote! {
                        vm.__module_set_attr(&module, #py_name, vm.new_pyobj(#ident(vm))).unwrap();
                    },
                )
            }
            Item::Const(syn::ItemConst { ident, .. }) => {
                let py_name = get_py_name(args.attrs, &ident)?;
                (
                    py_name.clone(),
                    quote! {
                        vm.__module_set_attr(&module, #py_name, vm.new_pyobj(#ident)).unwrap();
                    },
                )
            }
            Item::Use(syn::ItemUse {
                tree: UseTree::Path(path),
                ..
            }) => {
                let ident = match &*path.tree {
                    UseTree::Name(name) => &name.ident,
                    UseTree::Rename(rename) => &rename.rename,
                    other => {
                        return Err(self.new_syn_error(other.span(), "can only be a simple use"));
                    }
                };

                let py_name = get_py_name(args.attrs, &ident)?;
                (
                    py_name.clone(),
                    quote! {
                        vm.__module_set_attr(&module, #py_name, vm.new_pyobj(#ident)).unwrap();
                    },
                )
            }
            other => {
                return Err(
                    self.new_syn_error(other.span(), "can only be on a function, const and use")
                )
            }
        };
        args.attrs.remove(self.index());

        args.module
            .module_extend_items
            .add_item(py_name, args.cfgs.to_vec(), tokens)?;

        Ok(())
    }
}
