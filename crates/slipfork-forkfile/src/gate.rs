//! The single `on <expr>` feature gate.
//!
//! Every walker that traverses declarations (`boot_plan`, world planning, env evaluation)
//! descends through [`each_enabled`], so [`FeatureSet::matches`] runs exactly once per `on`
//! node and no downstream caller re-inspects `on` structure.

use crate::ast::{
    CommandItem, EnvItem, Hook, HookBody, ImageItem, On, OnBody, ServiceItem, TopItem,
};
use crate::eval::FeatureSet;
use crate::expr::Command;
use crate::resources::{PostgresItem, S3Item};

/// An AST item enum that can be guarded by an `on <expr>` node.
pub trait GatedItem: Sized {
    /// The `on` node carrying this item, when the item is one.
    fn gated(&self) -> Option<&On<Self>>;
}

macro_rules! gated_items {
    ($($item:ty),* $(,)?) => {
        $(
            impl GatedItem for $item {
                fn gated(&self) -> Option<&On<Self>> {
                    match self {
                        Self::On(on) => Some(on),
                        _ => None,
                    }
                }
            }
        )*
    };
}

gated_items!(
    TopItem,
    ImageItem,
    EnvItem,
    ServiceItem,
    PostgresItem,
    S3Item,
    CommandItem
);

/// Visits every item, descending into `on` bodies the feature set enables.
///
/// Items behind a disabled condition are never yielded, so callers see one already-gated
/// document view and must not match `On` themselves.
pub fn each_enabled<'a, I: GatedItem>(
    items: &'a [I],
    features: &FeatureSet,
    visit: &mut impl FnMut(&'a I),
) {
    for item in items {
        match item.gated() {
            Some(on) if features.matches(&on.cond) => match &on.body {
                OnBody::Inline(inner) => {
                    each_enabled(std::slice::from_ref(inner.as_ref()), features, visit);
                }
                OnBody::Block(block) => each_enabled(&block.items, features, visit),
            },
            Some(_) => {}
            None => visit(item),
        }
    }
}

impl Hook {
    /// The active commands in declaration order; nested `on` branches flatten recursively.
    ///
    /// Boot planning and runtime execution both read this, so the DAG and the pod agree.
    #[must_use]
    pub fn commands(&self, features: &FeatureSet) -> Vec<&Command> {
        match &self.body {
            HookBody::Inline(command) => vec![command],
            HookBody::Block(block) => {
                let mut commands = Vec::new();
                each_enabled(&block.items, features, &mut |item| {
                    if let CommandItem::Command(command) = item {
                        commands.push(command);
                    }
                });
                commands
            }
        }
    }
}
