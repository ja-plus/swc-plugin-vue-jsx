use crate::VueJsxTransformVisitor;
use indexmap::{IndexMap, IndexSet};
use swc_core::{
    common::{comments::Comments, EqIgnoreSpan, Spanned, DUMMY_SP},
    ecma::{
        ast::*,
        atoms::{js_word, JsWord},
        utils::quote_ident,
    },
    plugin::errors::HANDLER,
};

enum RefinedTsTypeElement {
    Property(TsPropertySignature),
    GetterSignature(TsGetterSignature),
    MethodSignature(TsMethodSignature),
}

struct PropIr {
    types: IndexSet<Option<JsWord>>,
    required: bool,
}

impl<C> VueJsxTransformVisitor<C>
where
    C: Comments,
{
    pub(crate) fn extract_props_type(&self, setup_fn: &ExprOrSpread) -> Option<ObjectLit> {
        let Some(first_param_type) = (if let ExprOrSpread { expr, spread: None } = setup_fn {
            match &**expr {
                Expr::Arrow(arrow) => match arrow.params.first() {
                    Some(Pat::Ident(ident)) => ident.type_ann.as_deref(),
                    Some(Pat::Array(array)) => array.type_ann.as_deref(),
                    Some(Pat::Object(object)) => object.type_ann.as_deref(),
                    _ => return None,
                },
                Expr::Fn(fn_expr) => {
                    match fn_expr.function.params.first().map(|param| &param.pat) {
                        Some(Pat::Ident(ident)) => ident.type_ann.as_deref(),
                        Some(Pat::Array(array)) => array.type_ann.as_deref(),
                        Some(Pat::Object(object)) => object.type_ann.as_deref(),
                        _ => return None,
                    }
                }
                _ => return None,
            }
        } else {
            return None;
        }) else {
            return None;
        };

        Some(self.build_props_type(first_param_type))
    }

    fn build_props_type(&self, TsTypeAnn { type_ann, .. }: &TsTypeAnn) -> ObjectLit {
        let mut props = Vec::with_capacity(3);
        self.resolve_props(type_ann, &mut props);

        let cap = props.len();
        let irs = props.into_iter().fold(
            IndexMap::<PropName, PropIr>::with_capacity(cap),
            |mut irs, prop| {
                match prop {
                    RefinedTsTypeElement::Property(TsPropertySignature {
                        key,
                        computed,
                        optional,
                        type_ann,
                        ..
                    })
                    | RefinedTsTypeElement::GetterSignature(TsGetterSignature {
                        key,
                        computed,
                        optional,
                        type_ann,
                        ..
                    }) => {
                        let prop_name = extract_prop_name(*key, computed);
                        let types = if let Some(type_ann) = type_ann {
                            self.infer_runtime_type(&type_ann.type_ann)
                        } else {
                            let mut types = IndexSet::with_capacity(1);
                            types.insert(None);
                            types
                        };
                        if let Some((_, ir)) = irs
                            .iter_mut()
                            .find(|(key, _)| prop_name.eq_ignore_span(key))
                        {
                            if optional {
                                ir.required = false;
                            }
                            ir.types.extend(types);
                        } else {
                            irs.insert(
                                prop_name,
                                PropIr {
                                    types,
                                    required: !optional,
                                },
                            );
                        }
                    }
                    RefinedTsTypeElement::MethodSignature(TsMethodSignature {
                        key,
                        computed,
                        optional,
                        ..
                    }) => {
                        let prop_name = extract_prop_name(*key, computed);
                        let ty = Some(js_word!("Function"));
                        if let Some((_, ir)) = irs
                            .iter_mut()
                            .find(|(key, _)| prop_name.eq_ignore_span(key))
                        {
                            if optional {
                                ir.required = false;
                            }
                            ir.types.insert(ty);
                        } else {
                            let mut types = IndexSet::with_capacity(1);
                            types.insert(ty);
                            irs.insert(
                                prop_name,
                                PropIr {
                                    types,
                                    required: !optional,
                                },
                            );
                        }
                    }
                }
                irs
            },
        );

        ObjectLit {
            props: irs
                .into_iter()
                .map(|(prop_name, mut ir)| {
                    PropOrSpread::Prop(Box::new(Prop::KeyValue(KeyValueProp {
                        key: prop_name,
                        value: Box::new(Expr::Object(ObjectLit {
                            props: vec![
                                PropOrSpread::Prop(Box::new(Prop::KeyValue(KeyValueProp {
                                    key: PropName::Ident(quote_ident!("type")),
                                    value: Box::new(if ir.types.len() == 1 {
                                        if let Some(ty) = ir.types.pop().unwrap() {
                                            Expr::Ident(quote_ident!(ty))
                                        } else {
                                            Expr::Lit(Lit::Null(Null { span: DUMMY_SP }))
                                        }
                                    } else {
                                        Expr::Array(ArrayLit {
                                            elems: ir
                                                .types
                                                .into_iter()
                                                .map(|ty| {
                                                    Some(ExprOrSpread {
                                                        expr: Box::new(if let Some(ty) = ty {
                                                            Expr::Ident(quote_ident!(ty))
                                                        } else {
                                                            Expr::Lit(Lit::Null(Null {
                                                                span: DUMMY_SP,
                                                            }))
                                                        }),
                                                        spread: None,
                                                    })
                                                })
                                                .collect(),
                                            span: DUMMY_SP,
                                        })
                                    }),
                                }))),
                                PropOrSpread::Prop(Box::new(Prop::KeyValue(KeyValueProp {
                                    key: PropName::Ident(quote_ident!("required")),
                                    value: Box::new(Expr::Lit(Lit::Bool(Bool {
                                        value: ir.required,
                                        span: DUMMY_SP,
                                    }))),
                                }))),
                            ],
                            span: DUMMY_SP,
                        })),
                    })))
                })
                .collect(),
            span: DUMMY_SP,
        }
    }

    fn resolve_props(&self, ty: &TsType, props: &mut Vec<RefinedTsTypeElement>) {
        match ty {
            TsType::TsTypeLit(TsTypeLit { members, .. }) => {
                props.extend(members.iter().filter_map(|member| match member {
                    TsTypeElement::TsPropertySignature(prop) => {
                        Some(RefinedTsTypeElement::Property(prop.clone()))
                    }
                    TsTypeElement::TsMethodSignature(method) => {
                        Some(RefinedTsTypeElement::MethodSignature(method.clone()))
                    }
                    TsTypeElement::TsGetterSignature(getter) => {
                        Some(RefinedTsTypeElement::GetterSignature(getter.clone()))
                    }
                    _ => None,
                }));
            }
            TsType::TsUnionOrIntersectionType(
                TsUnionOrIntersectionType::TsIntersectionType(TsIntersectionType { types, .. })
                | TsUnionOrIntersectionType::TsUnionType(TsUnionType { types, .. }),
            ) => {
                types.iter().for_each(|ty| self.resolve_props(ty, props));
            }
            TsType::TsTypeRef(TsTypeRef {
                type_name: TsEntityName::Ident(ident),
                type_params,
                span,
                ..
            }) => {
                let key = (ident.sym.clone(), ident.span.ctxt());
                if let Some(aliased) = self.type_aliases.get(&key) {
                    self.resolve_props(aliased, props);
                } else if let Some(TsInterfaceDecl {
                    extends,
                    body: TsInterfaceBody { body, .. },
                    ..
                }) = self.interfaces.get(&key)
                {
                    props.extend(body.iter().filter_map(|element| match element {
                        TsTypeElement::TsPropertySignature(prop) => {
                            Some(RefinedTsTypeElement::Property(prop.clone()))
                        }
                        TsTypeElement::TsMethodSignature(method) => {
                            Some(RefinedTsTypeElement::MethodSignature(method.clone()))
                        }
                        TsTypeElement::TsGetterSignature(getter) => {
                            Some(RefinedTsTypeElement::GetterSignature(getter.clone()))
                        }
                        _ => None,
                    }));
                    extends
                        .iter()
                        .filter_map(|parent| parent.expr.as_ident())
                        .for_each(|ident| {
                            self.resolve_props(
                                &TsType::TsTypeRef(TsTypeRef {
                                    type_name: TsEntityName::Ident(ident.clone()),
                                    type_params: None,
                                    span: DUMMY_SP,
                                }),
                                props,
                            )
                        });
                } else if ident.span.ctxt().has_mark(self.unresolved_mark) {
                    match &*ident.sym {
                        "Partial" => {
                            if let Some(param) = type_params
                                .as_deref()
                                .and_then(|params| params.params.first())
                            {
                                let mut inner_props = vec![];
                                self.resolve_props(param, &mut inner_props);
                                props.extend(inner_props.into_iter().map(|mut prop| {
                                    match &mut prop {
                                        RefinedTsTypeElement::Property(property) => {
                                            property.optional = true;
                                        }
                                        RefinedTsTypeElement::MethodSignature(method) => {
                                            method.optional = true;
                                        }
                                        RefinedTsTypeElement::GetterSignature(getter) => {
                                            getter.optional = true;
                                        }
                                    }
                                    prop
                                }));
                            }
                        }
                        "Required" => {
                            if let Some(param) = type_params
                                .as_deref()
                                .and_then(|params| params.params.first())
                            {
                                let mut inner_props = vec![];
                                self.resolve_props(param, &mut inner_props);
                                props.extend(inner_props.into_iter().map(|mut prop| {
                                    match &mut prop {
                                        RefinedTsTypeElement::Property(TsPropertySignature {
                                            optional,
                                            ..
                                        })
                                        | RefinedTsTypeElement::MethodSignature(
                                            TsMethodSignature { optional, .. },
                                        )
                                        | RefinedTsTypeElement::GetterSignature(
                                            TsGetterSignature { optional, .. },
                                        ) => {
                                            *optional = false;
                                        }
                                    }
                                    prop
                                }));
                            }
                        }
                        "Pick" => {
                            if let Some((object, keys)) = type_params
                                .as_deref()
                                .and_then(|params| params.params.first().zip(params.params.get(1)))
                            {
                                let keys = self.resolve_index_keys(keys);
                                let mut inner_props = vec![];
                                self.resolve_props(object, &mut inner_props);
                                props.extend(inner_props.into_iter().filter(|prop| match prop {
                                    RefinedTsTypeElement::Property(TsPropertySignature {
                                        key,
                                        ..
                                    })
                                    | RefinedTsTypeElement::MethodSignature(TsMethodSignature {
                                        key,
                                        ..
                                    })
                                    | RefinedTsTypeElement::GetterSignature(TsGetterSignature {
                                        key,
                                        ..
                                    }) => match &**key {
                                        Expr::Ident(ident) => keys.contains(&ident.sym),
                                        Expr::Lit(Lit::Str(str)) => keys.contains(&str.value),
                                        _ => false,
                                    },
                                }));
                            }
                        }
                        "Omit" => {
                            if let Some((object, keys)) = type_params
                                .as_deref()
                                .and_then(|params| params.params.first().zip(params.params.get(1)))
                            {
                                let keys = self.resolve_index_keys(keys);
                                let mut inner_props = vec![];
                                self.resolve_props(object, &mut inner_props);
                                props.extend(inner_props.into_iter().filter(|prop| match prop {
                                    RefinedTsTypeElement::Property(TsPropertySignature {
                                        key,
                                        ..
                                    })
                                    | RefinedTsTypeElement::MethodSignature(TsMethodSignature {
                                        key,
                                        ..
                                    })
                                    | RefinedTsTypeElement::GetterSignature(TsGetterSignature {
                                        key,
                                        ..
                                    }) => match &**key {
                                        Expr::Ident(ident) => !keys.contains(&ident.sym),
                                        Expr::Lit(Lit::Str(str)) => !keys.contains(&str.value),
                                        _ => true,
                                    },
                                }));
                            }
                        }
                        _ => {
                            HANDLER.with(|handler| {
                                handler.span_err(
                                    *span,
                                    "Unresolvable type reference or unsupported built-in utility type.",
                                );
                            });
                        }
                    }
                } else {
                    HANDLER.with(|handler| {
                        handler.span_err(*span, "Types from other modules can't be resolved.");
                    });
                }
            }
            TsType::TsIndexedAccessType(TsIndexedAccessType {
                obj_type,
                index_type,
                ..
            }) => {
                if let Some(ty) = self.resolve_indexed_access(obj_type, index_type) {
                    self.resolve_props(&ty, props);
                } else {
                    HANDLER.with(|handler| {
                        handler.span_err(ty.span(), "Unresolvable type.");
                    });
                }
            }
            TsType::TsParenthesizedType(TsParenthesizedType { type_ann, .. })
            | TsType::TsOptionalType(TsOptionalType { type_ann, .. }) => {
                self.resolve_props(type_ann, props);
            }
            _ => HANDLER.with(|handler| {
                handler.span_err(ty.span(), "Unresolvable type.");
            }),
        }
    }

    fn resolve_index_keys(&self, ty: &TsType) -> Vec<JsWord> {
        match ty {
            TsType::TsLitType(TsLitType {
                lit: TsLit::Str(key),
                ..
            }) => vec![key.value.clone()],
            TsType::TsUnionOrIntersectionType(TsUnionOrIntersectionType::TsUnionType(
                TsUnionType { types, .. },
            )) => types
                .iter()
                .filter_map(|ty| ty.as_ts_lit_type().and_then(|lit| lit.lit.as_str()))
                .map(|str| str.value.clone())
                .collect(),
            TsType::TsTypeRef(TsTypeRef {
                type_name: TsEntityName::Ident(ident),
                ..
            }) => {
                if let Some(aliased) = self
                    .type_aliases
                    .get(&(ident.sym.clone(), ident.span.ctxt()))
                {
                    self.resolve_index_keys(aliased)
                } else if ident.span.ctxt().has_mark(self.unresolved_mark) {
                    HANDLER.with(|handler| {
                        handler.span_err(
                            ty.span(),
                            "Unresolvable type reference or unsupported built-in utility type.",
                        );
                    });
                    vec![]
                } else {
                    HANDLER.with(|handler| {
                        handler.span_err(ty.span(), "Types from other modules can't be resolved.");
                    });
                    vec![]
                }
            }
            _ => {
                HANDLER
                    .with(|handler| handler.span_err(ty.span(), "Unsupported type as index key."));
                vec![]
            }
        }
    }

    fn resolve_indexed_access(&self, obj: &TsType, index: &TsType) -> Option<TsType> {
        match obj {
            TsType::TsTypeRef(TsTypeRef {
                type_name: TsEntityName::Ident(ident),
                type_params,
                ..
            }) => {
                let key = (ident.sym.clone(), ident.span.ctxt());
                if let Some(aliased) = self.type_aliases.get(&key) {
                    self.resolve_indexed_access(aliased, index)
                } else if let Some(interface) = self.interfaces.get(&key) {
                    let mut properties = match index {
                        TsType::TsKeywordType(TsKeywordType {
                            kind: TsKeywordTypeKind::TsStringKeyword,
                            ..
                        }) => interface
                            .body
                            .body
                            .iter()
                            .filter_map(|element| match element {
                                TsTypeElement::TsCallSignatureDecl(..)
                                | TsTypeElement::TsConstructSignatureDecl(..)
                                | TsTypeElement::TsSetterSignature(..) => None,
                                TsTypeElement::TsPropertySignature(TsPropertySignature {
                                    key,
                                    type_ann,
                                    ..
                                })
                                | TsTypeElement::TsGetterSignature(TsGetterSignature {
                                    key,
                                    type_ann,
                                    ..
                                }) => {
                                    if matches!(&**key, Expr::Ident(..) | Expr::Lit(Lit::Str(..))) {
                                        type_ann.as_ref().map(|type_ann| type_ann.type_ann.clone())
                                    } else {
                                        None
                                    }
                                }
                                TsTypeElement::TsIndexSignature(TsIndexSignature {
                                    type_ann,
                                    ..
                                }) => type_ann.as_ref().map(|type_ann| type_ann.type_ann.clone()),
                                TsTypeElement::TsMethodSignature(..) => {
                                    Some(Box::new(TsType::TsTypeRef(TsTypeRef {
                                        type_name: TsEntityName::Ident(quote_ident!("Function")),
                                        type_params: None,
                                        span: DUMMY_SP,
                                    })))
                                }
                            })
                            .collect(),
                        TsType::TsLitType(TsLitType {
                            lit: TsLit::Str(..),
                            ..
                        })
                        | TsType::TsUnionOrIntersectionType(
                            TsUnionOrIntersectionType::TsUnionType(..),
                        )
                        | TsType::TsTypeRef(..) => {
                            let keys = self.resolve_index_keys(index);
                            interface
                                .body
                                .body
                                .iter()
                                .filter_map(|element| match element {
                                    TsTypeElement::TsPropertySignature(TsPropertySignature {
                                        key,
                                        type_ann,
                                        ..
                                    })
                                    | TsTypeElement::TsGetterSignature(TsGetterSignature {
                                        key,
                                        type_ann,
                                        ..
                                    }) => {
                                        if let Expr::Ident(Ident { sym: key, .. })
                                        | Expr::Lit(Lit::Str(Str { value: key, .. })) = &**key
                                        {
                                            if keys.contains(key) {
                                                type_ann
                                                    .as_ref()
                                                    .map(|type_ann| type_ann.type_ann.clone())
                                            } else {
                                                None
                                            }
                                        } else {
                                            None
                                        }
                                    }
                                    TsTypeElement::TsMethodSignature(TsMethodSignature {
                                        key,
                                        ..
                                    }) => {
                                        if let Expr::Ident(Ident { sym: key, .. })
                                        | Expr::Lit(Lit::Str(Str { value: key, .. })) = &**key
                                        {
                                            if keys.contains(key) {
                                                Some(Box::new(TsType::TsTypeRef(TsTypeRef {
                                                    type_name: TsEntityName::Ident(quote_ident!(
                                                        "Function"
                                                    )),
                                                    type_params: None,
                                                    span: DUMMY_SP,
                                                })))
                                            } else {
                                                None
                                            }
                                        } else {
                                            None
                                        }
                                    }
                                    TsTypeElement::TsCallSignatureDecl(..)
                                    | TsTypeElement::TsConstructSignatureDecl(..)
                                    | TsTypeElement::TsSetterSignature(..)
                                    | TsTypeElement::TsIndexSignature(..) => None,
                                })
                                .collect()
                        }
                        _ => vec![],
                    };
                    if properties.len() == 1 {
                        Some((*properties.remove(0)).clone())
                    } else {
                        Some(TsType::TsUnionOrIntersectionType(
                            TsUnionOrIntersectionType::TsUnionType(TsUnionType {
                                types: properties,
                                span: DUMMY_SP,
                            }),
                        ))
                    }
                } else if ident.span.ctxt().has_mark(self.unresolved_mark) {
                    if ident.sym == "Array" {
                        type_params
                            .as_ref()
                            .and_then(|params| params.params.first())
                            .map(|ty| (**ty).clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            TsType::TsTypeLit(TsTypeLit { members, .. }) => {
                let mut properties = match index {
                    TsType::TsKeywordType(TsKeywordType {
                        kind: TsKeywordTypeKind::TsStringKeyword,
                        ..
                    }) => members
                        .iter()
                        .filter_map(|member| match member {
                            TsTypeElement::TsCallSignatureDecl(..)
                            | TsTypeElement::TsConstructSignatureDecl(..)
                            | TsTypeElement::TsSetterSignature(..) => None,
                            TsTypeElement::TsPropertySignature(TsPropertySignature {
                                key,
                                type_ann,
                                ..
                            })
                            | TsTypeElement::TsGetterSignature(TsGetterSignature {
                                key,
                                type_ann,
                                ..
                            }) => {
                                if matches!(&**key, Expr::Ident(..) | Expr::Lit(Lit::Str(..))) {
                                    type_ann.as_ref().map(|type_ann| type_ann.type_ann.clone())
                                } else {
                                    None
                                }
                            }
                            TsTypeElement::TsIndexSignature(TsIndexSignature {
                                type_ann, ..
                            }) => type_ann.as_ref().map(|type_ann| type_ann.type_ann.clone()),
                            TsTypeElement::TsMethodSignature(..) => {
                                Some(Box::new(TsType::TsTypeRef(TsTypeRef {
                                    type_name: TsEntityName::Ident(quote_ident!("Function")),
                                    type_params: None,
                                    span: DUMMY_SP,
                                })))
                            }
                        })
                        .collect(),
                    TsType::TsLitType(TsLitType {
                        lit: TsLit::Str(..),
                        ..
                    })
                    | TsType::TsTypeRef(..)
                    | TsType::TsUnionOrIntersectionType(TsUnionOrIntersectionType::TsUnionType(
                        ..,
                    )) => {
                        let keys = self.resolve_index_keys(index);
                        members
                            .iter()
                            .filter_map(|member| match member {
                                TsTypeElement::TsPropertySignature(TsPropertySignature {
                                    key,
                                    type_ann,
                                    ..
                                })
                                | TsTypeElement::TsGetterSignature(TsGetterSignature {
                                    key,
                                    type_ann,
                                    ..
                                }) => {
                                    if let Expr::Ident(Ident { sym: key, .. })
                                    | Expr::Lit(Lit::Str(Str { value: key, .. })) = &**key
                                    {
                                        if keys.contains(key) {
                                            type_ann
                                                .as_ref()
                                                .map(|type_ann| type_ann.type_ann.clone())
                                        } else {
                                            None
                                        }
                                    } else {
                                        None
                                    }
                                }
                                TsTypeElement::TsMethodSignature(TsMethodSignature {
                                    key, ..
                                }) => {
                                    if let Expr::Ident(Ident { sym: key, .. })
                                    | Expr::Lit(Lit::Str(Str { value: key, .. })) = &**key
                                    {
                                        if keys.contains(key) {
                                            Some(Box::new(TsType::TsTypeRef(TsTypeRef {
                                                type_name: TsEntityName::Ident(quote_ident!(
                                                    "Function"
                                                )),
                                                type_params: None,
                                                span: DUMMY_SP,
                                            })))
                                        } else {
                                            None
                                        }
                                    } else {
                                        None
                                    }
                                }
                                TsTypeElement::TsCallSignatureDecl(..)
                                | TsTypeElement::TsConstructSignatureDecl(..)
                                | TsTypeElement::TsSetterSignature(..)
                                | TsTypeElement::TsIndexSignature(..) => None,
                            })
                            .collect()
                    }
                    _ => vec![],
                };
                if properties.len() == 1 {
                    Some(*properties.remove(0))
                } else {
                    Some(TsType::TsUnionOrIntersectionType(
                        TsUnionOrIntersectionType::TsUnionType(TsUnionType {
                            types: properties,
                            span: DUMMY_SP,
                        }),
                    ))
                }
            }
            TsType::TsArrayType(TsArrayType { elem_type, .. }) => {
                if matches!(
                    index,
                    TsType::TsKeywordType(TsKeywordType {
                        kind: TsKeywordTypeKind::TsNumberKeyword,
                        ..
                    }) | TsType::TsLitType(TsLitType {
                        lit: TsLit::Number(..),
                        ..
                    })
                ) {
                    Some((**elem_type).clone())
                } else {
                    None
                }
            }
            TsType::TsTupleType(TsTupleType { elem_types, .. }) => match index {
                TsType::TsLitType(TsLitType {
                    lit: TsLit::Number(num),
                    ..
                }) => elem_types
                    .get(num.value as usize)
                    .map(|element| (*element.ty).clone()),
                TsType::TsKeywordType(TsKeywordType {
                    kind: TsKeywordTypeKind::TsNumberKeyword,
                    ..
                }) => Some(TsType::TsUnionOrIntersectionType(
                    TsUnionOrIntersectionType::TsUnionType(TsUnionType {
                        types: elem_types
                            .iter()
                            .map(|TsTupleElement { ty, .. }| ty.clone())
                            .collect(),
                        span: DUMMY_SP,
                    }),
                )),
                _ => None,
            },
            _ => None,
        }
    }

    fn infer_runtime_type(&self, ty: &TsType) -> IndexSet<Option<JsWord>> {
        let mut runtime_types = IndexSet::with_capacity(1);
        match ty {
            TsType::TsKeywordType(keyword) => match keyword.kind {
                TsKeywordTypeKind::TsStringKeyword => {
                    runtime_types.insert(Some(js_word!("String")));
                }
                TsKeywordTypeKind::TsNumberKeyword => {
                    runtime_types.insert(Some(js_word!("Number")));
                }
                TsKeywordTypeKind::TsBooleanKeyword => {
                    runtime_types.insert(Some(js_word!("Boolean")));
                }
                TsKeywordTypeKind::TsObjectKeyword => {
                    runtime_types.insert(Some(js_word!("Object")));
                }
                TsKeywordTypeKind::TsNullKeyword => {
                    runtime_types.insert(None);
                }
                TsKeywordTypeKind::TsBigIntKeyword => {
                    runtime_types.insert(Some(js_word!("BigInt")));
                }
                TsKeywordTypeKind::TsSymbolKeyword => {
                    runtime_types.insert(Some(js_word!("Symbol")));
                }
                _ => {
                    runtime_types.insert(None);
                }
            },
            TsType::TsTypeLit(TsTypeLit { members, .. }) => {
                members.iter().for_each(|member| {
                    if let TsTypeElement::TsCallSignatureDecl(..)
                    | TsTypeElement::TsConstructSignatureDecl(..) = member
                    {
                        runtime_types.insert(Some(js_word!("Function")));
                    } else {
                        runtime_types.insert(Some(js_word!("Object")));
                    }
                });
            }
            TsType::TsFnOrConstructorType(..) => {
                runtime_types.insert(Some(js_word!("Function")));
            }
            TsType::TsArrayType(..) | TsType::TsTupleType(..) => {
                runtime_types.insert(Some(js_word!("Array")));
            }
            TsType::TsLitType(TsLitType { lit, .. }) => match lit {
                TsLit::Str(..) | TsLit::Tpl(..) => {
                    runtime_types.insert(Some(js_word!("String")));
                }
                TsLit::Bool(..) => {
                    runtime_types.insert(Some(js_word!("Boolean")));
                }
                TsLit::Number(..) | TsLit::BigInt(..) => {
                    runtime_types.insert(Some(js_word!("Number")));
                }
            },
            TsType::TsTypeRef(TsTypeRef {
                type_name: TsEntityName::Ident(ident),
                type_params,
                ..
            }) => {
                let key = (ident.sym.clone(), ident.span.ctxt());
                if let Some(aliased) = self.type_aliases.get(&key) {
                    runtime_types.extend(self.infer_runtime_type(aliased));
                } else if let Some(TsInterfaceDecl {
                    body: TsInterfaceBody { body, .. },
                    ..
                }) = self.interfaces.get(&key)
                {
                    body.iter().for_each(|element| {
                        if let TsTypeElement::TsCallSignatureDecl(..)
                        | TsTypeElement::TsConstructSignatureDecl(..) = element
                        {
                            runtime_types.insert(Some(js_word!("Function")));
                        } else {
                            runtime_types.insert(Some(js_word!("Object")));
                        }
                    });
                } else {
                    match &*ident.sym {
                        "Array" | "Function" | "Object" | "Set" | "Map" | "WeakSet" | "WeakMap"
                        | "Date" | "Promise" | "Error" | "RegExp" => {
                            runtime_types.insert(Some(ident.sym.clone()));
                        }
                        "Partial" | "Required" | "Readonly" | "Record" | "Pick" | "Omit"
                        | "InstanceType" => {
                            runtime_types.insert(Some(js_word!("Object")));
                        }
                        "Uppercase" | "Lowercase" | "Capitalize" | "Uncapitalize" => {
                            runtime_types.insert(Some(js_word!("String")));
                        }
                        "Parameters" | "ConstructorParameters" => {
                            runtime_types.insert(Some(js_word!("Array")));
                        }
                        "NonNullable" => {
                            if let Some(ty) = type_params
                                .as_ref()
                                .and_then(|type_params| type_params.params.first())
                            {
                                let types = self.infer_runtime_type(ty);
                                runtime_types.extend(types.into_iter().filter(|ty| ty.is_some()));
                            } else {
                                runtime_types.insert(Some(js_word!("Object")));
                            }
                        }
                        "Exclude" | "OmitThisParameter" => {
                            if let Some(ty) = type_params
                                .as_ref()
                                .and_then(|type_params| type_params.params.first())
                            {
                                runtime_types.extend(self.infer_runtime_type(ty));
                            } else {
                                runtime_types.insert(Some(js_word!("Object")));
                            }
                        }
                        "Extract" => {
                            if let Some(ty) = type_params
                                .as_ref()
                                .and_then(|type_params| type_params.params.get(1))
                            {
                                runtime_types.extend(self.infer_runtime_type(ty));
                            } else {
                                runtime_types.insert(Some(js_word!("Object")));
                            }
                        }
                        _ => {
                            runtime_types.insert(Some(js_word!("Object")));
                        }
                    }
                }
            }
            TsType::TsParenthesizedType(TsParenthesizedType { type_ann, .. }) => {
                runtime_types.extend(self.infer_runtime_type(type_ann));
            }
            TsType::TsUnionOrIntersectionType(
                TsUnionOrIntersectionType::TsUnionType(TsUnionType { types, .. })
                | TsUnionOrIntersectionType::TsIntersectionType(TsIntersectionType { types, .. }),
            ) => runtime_types.extend(types.iter().flat_map(|ty| self.infer_runtime_type(ty))),
            TsType::TsIndexedAccessType(TsIndexedAccessType {
                obj_type,
                index_type,
                ..
            }) => {
                if let Some(ty) = self.resolve_indexed_access(obj_type, index_type) {
                    runtime_types.extend(self.infer_runtime_type(&ty));
                }
            }
            TsType::TsOptionalType(TsOptionalType { type_ann, .. }) => {
                runtime_types.extend(self.infer_runtime_type(type_ann));
            }
            _ => {
                runtime_types.insert(Some(js_word!("Object")));
            }
        };
        runtime_types
    }
}

fn extract_prop_name(expr: Expr, computed: bool) -> PropName {
    if computed {
        PropName::Computed(ComputedPropName {
            expr: Box::new(expr),
            span: DUMMY_SP,
        })
    } else {
        match expr {
            Expr::Ident(ident) => PropName::Ident(ident),
            Expr::Lit(Lit::Str(str)) => PropName::Str(str),
            Expr::Lit(Lit::Num(num)) => PropName::Num(num),
            Expr::Lit(Lit::BigInt(bigint)) => PropName::BigInt(bigint),
            _ => {
                HANDLER.with(|handler| handler.span_err(expr.span(), "Unsupported prop key."));
                PropName::Ident(quote_ident!(""))
            }
        }
    }
}
