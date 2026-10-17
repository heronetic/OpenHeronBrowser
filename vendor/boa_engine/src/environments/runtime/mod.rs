use crate::{
    Context, JsResult, JsString, JsSymbol, JsValue,
    object::{JsObject, PrivateName},
};
use boa_ast::scope::{BindingLocator, BindingLocatorScope, Scope};
use boa_gc::{Finalize, Gc, Trace};
use thin_vec::ThinVec;

mod declarative;
mod private;

use self::declarative::ModuleEnvironment;
pub(crate) use self::{
    declarative::{
        DeclarativeEnvironment, DeclarativeEnvironmentKind, FunctionEnvironment, FunctionSlots,
        LexicalEnvironment, ThisBindingStatus,
    },
    private::PrivateEnvironment,
};

/// A single node in the environment chain.
///
/// Each node holds one [`Environment`] and a pointer to its parent.
/// The chain is immutable from the perspective of other holders — pushing
/// creates a new tip node, leaving existing nodes untouched. This makes
/// cloning the chain O(1) (a single `Gc` ref-count bump).
#[derive(Clone, Debug, Trace, Finalize)]
pub(crate) struct EnvironmentNode {
    env: Environment,
    parent: Option<Gc<EnvironmentNode>>,
}

/// The environment stack holds all environments at runtime.
///
/// Implemented as a singly-linked list of [`EnvironmentNode`]s, where each
/// node points to its parent (the previous environment). Cloning is O(1) —
/// just a reference-count bump on the tip pointer plus small scalar copies.
///
/// The global declarative environment is NOT stored here — it lives in the
/// [`crate::realm::Realm`] and is accessed via `frame.realm.environment()`.
/// This avoids a `Gc` clone on every function call.
#[derive(Clone, Debug, Trace, Finalize)]
pub(crate) struct EnvironmentStack {
    /// The frozen part of the chain (shared with closures that captured it).
    tip: Option<Gc<EnvironmentNode>>,

    /// OHB: environments pushed since the chain was last frozen, oldest
    /// first. They become linked nodes only when a closure captures the
    /// stack, so calls that create no closures allocate no chain nodes.
    local: ThinVec<Environment>,

    /// Number of environments in the stack (not counting global).
    #[unsafe_ignore_trace]
    depth: u32,

    private_stack: ThinVec<Gc<PrivateEnvironment>>,
}

/// Saved environment state for `pop_to_global` / `restore_from_saved`.
/// Used by indirect `eval` and `Function.prototype.toString` recompilation.
pub(crate) struct SavedEnvironments {
    tip: Option<Gc<EnvironmentNode>>,
    local: ThinVec<Environment>,
    depth: u32,
}

/// A runtime environment.
#[derive(Clone, Debug, Trace, Finalize)]
pub(crate) enum Environment {
    Declarative(Gc<DeclarativeEnvironment>),
    Object(JsObject),
}

impl Environment {
    /// Returns the declarative environment if it is one.
    pub(crate) const fn as_declarative(&self) -> Option<&Gc<DeclarativeEnvironment>> {
        match self {
            Self::Declarative(env) => Some(env),
            Self::Object(_) => None,
        }
    }
}

impl EnvironmentStack {
    /// Create a new environment stack.
    pub(crate) fn new() -> Self {
        Self {
            tip: None,
            local: ThinVec::new(),
            depth: 0,
            private_stack: ThinVec::new(),
        }
    }

    /// Gets the next outer function environment.
    pub(crate) fn outer_function_environment(&self) -> Option<(Gc<DeclarativeEnvironment>, Scope)> {
        for (env, _) in self.iter_from_tip() {
            if let Some(decl) = env.as_declarative()
                && let Some(function_env) = decl.kind().as_function()
            {
                return Some((decl.clone(), function_env.compile().clone()));
            }
        }
        None
    }

    /// Save all current environments and clear the stack.
    /// Used by indirect eval and `Function.prototype.toString` recompilation.
    pub(crate) fn pop_to_global(&mut self) -> SavedEnvironments {
        SavedEnvironments {
            tip: self.tip.take(),
            local: std::mem::take(&mut self.local),
            depth: std::mem::replace(&mut self.depth, 0),
        }
    }

    /// Restore environments from a previous `pop_to_global` call.
    pub(crate) fn restore_from_saved(&mut self, saved: SavedEnvironments) {
        self.tip = saved.tip;
        self.local = saved.local;
        self.depth = saved.depth;
    }

    /// Get the number of current environments (not counting global).
    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.depth as usize
    }

    /// Get the environment at the given absolute index (0-based from root).
    ///
    /// Index 0 is the deepest (first pushed) environment in the chain.
    /// Index `len() - 1` is the tip (most recently pushed).
    #[inline]
    pub(crate) fn get(&self, index: usize) -> Option<&Environment> {
        let depth = self.depth as usize;
        if index >= depth {
            return None;
        }
        let base = depth - self.local.len();
        if index >= base {
            return self.local.get(index - base);
        }
        let steps = base - 1 - index;
        let mut current = self.tip.as_deref()?;
        for _ in 0..steps {
            current = current.parent.as_deref()?;
        }
        Some(&current.env)
    }

    /// Iterate from tip (most recent) toward root (oldest).
    /// Yields `(&Environment, absolute_index)`.
    fn iter_from_tip(&self) -> EnvironmentChainIter<'_> {
        EnvironmentChainIter {
            local: &self.local,
            current: self.tip.as_deref(),
            index: self.depth,
        }
    }

    /// Get the tip (most recently pushed) environment.
    #[inline]
    fn last(&self) -> Option<&Environment> {
        match self.local.last() {
            Some(e) => Some(e),
            None => self.tip.as_deref().map(|node| &node.env),
        }
    }

    /// Truncate current environments to the given depth.
    pub(crate) fn truncate(&mut self, len: usize) {
        while self.depth as usize > len {
            if self.local.pop().is_none() {
                let node = self.tip.as_ref().expect("depth > 0 implies tip is Some");
                self.tip = node.parent.clone();
            }
            self.depth -= 1;
        }
    }

    /// `GetThisEnvironment`
    ///
    /// Returns the environment that currently provides a `this` biding.
    ///
    /// More information:
    ///  - [ECMAScript specification][spec]
    ///
    /// [spec]: https://tc39.es/ecma262/#sec-getthisenvironment
    pub(crate) fn get_this_environment<'a>(
        &'a self,
        global: &'a Gc<DeclarativeEnvironment>,
    ) -> &'a DeclarativeEnvironmentKind {
        for (env, _) in self.iter_from_tip() {
            if let Some(decl) = env.as_declarative().filter(|decl| decl.has_this_binding()) {
                return decl.kind();
            }
        }

        global.kind()
    }

    /// `GetThisBinding`
    ///
    /// Returns the current `this` binding of the environment.
    /// Note: If the current environment is the global environment, this function returns `Ok(None)`.
    ///
    /// More information:
    ///  - [ECMAScript specification][spec]
    ///
    /// [spec]: https://tc39.es/ecma262/#sec-function-environment-records-getthisbinding
    pub(crate) fn get_this_binding(&self) -> JsResult<Option<JsValue>> {
        for (env, _) in self.iter_from_tip() {
            if let Environment::Declarative(decl) = env
                && let Some(this) = decl.get_this_binding()?
            {
                return Ok(Some(this));
            }
        }

        Ok(None)
    }

    /// Push a new object environment on the environments stack.
    pub(crate) fn push_object(&mut self, object: JsObject) {
        self.push_env(Environment::Object(object));
    }

    /// Push a lexical environment on the environments stack and return it's index.
    pub(crate) fn push_lexical(
        &mut self,
        bindings_count: u32,
        global: &Gc<DeclarativeEnvironment>,
    ) -> u32 {
        let (poisoned, with) = self.compute_poisoned_with(global);

        let index = self.depth;

        self.push_env(Environment::Declarative(Gc::new(
            DeclarativeEnvironment::new(
                DeclarativeEnvironmentKind::Lexical(LexicalEnvironment::new(bindings_count)),
                poisoned,
                with,
            ),
        )));

        index
    }

    /// Push a function environment on the environments stack.
    pub(crate) fn push_function(
        &mut self,
        scope: Scope,
        function_slots: FunctionSlots,
        global: &Gc<DeclarativeEnvironment>,
    ) {
        let num_bindings = scope.num_bindings_non_local();

        let (poisoned, with) = self.compute_poisoned_with(global);

        self.push_env(Environment::Declarative(Gc::new(
            DeclarativeEnvironment::new(
                DeclarativeEnvironmentKind::Function(FunctionEnvironment::new(
                    num_bindings,
                    function_slots,
                    scope,
                )),
                poisoned,
                with,
            ),
        )));
    }

    /// Push a module environment on the environments stack.
    pub(crate) fn push_module(&mut self, scope: Scope) {
        let num_bindings = scope.num_bindings_non_local();
        self.push_env(Environment::Declarative(Gc::new(
            DeclarativeEnvironment::new(
                DeclarativeEnvironmentKind::Module(ModuleEnvironment::new(num_bindings, scope)),
                false,
                false,
            ),
        )));
    }

    /// Pop environment from the environments stack.
    #[track_caller]
    pub(crate) fn pop(&mut self) {
        if self.local.pop().is_none() {
            let node = self
                .tip
                .as_ref()
                .expect("cannot pop empty environment chain");
            self.tip = node.parent.clone();
        }
        self.depth -= 1;
    }

    /// Get the most outer environment.
    pub(crate) fn current_declarative_ref<'a>(
        &'a self,
        global: &'a Gc<DeclarativeEnvironment>,
    ) -> Option<&'a Gc<DeclarativeEnvironment>> {
        if let Some(env) = self.last() {
            env.as_declarative()
        } else {
            Some(global)
        }
    }

    /// Mark that there may be added bindings from the current environment to the next function
    /// environment.
    pub(crate) fn poison_until_last_function(&mut self, global: &Gc<DeclarativeEnvironment>) {
        for (env, _) in self.iter_from_tip() {
            if let Some(decl) = env.as_declarative() {
                decl.poison();
                if decl.is_function() {
                    return;
                }
            }
        }
        global.poison();
    }

    /// Set the value of a lexical binding.
    ///
    /// # Panics
    ///
    /// Panics if the environment or binding index are out of range.
    #[track_caller]
    pub(crate) fn put_lexical_value(
        &mut self,
        environment: BindingLocatorScope,
        binding_index: u32,
        value: JsValue,
        global: &Gc<DeclarativeEnvironment>,
    ) {
        let env = match environment {
            BindingLocatorScope::GlobalObject | BindingLocatorScope::GlobalDeclarative => global,
            BindingLocatorScope::Stack(index) => self
                .get(index as usize)
                .and_then(Environment::as_declarative)
                .expect("must be declarative environment"),
        };
        env.set(binding_index, value);
    }

    /// Set the value of a binding if it is uninitialized.
    ///
    /// # Panics
    ///
    /// Panics if the environment or binding index are out of range.
    #[track_caller]
    pub(crate) fn put_value_if_uninitialized(
        &mut self,
        environment: BindingLocatorScope,
        binding_index: u32,
        value: JsValue,
        global: &Gc<DeclarativeEnvironment>,
    ) {
        let env = match environment {
            BindingLocatorScope::GlobalObject | BindingLocatorScope::GlobalDeclarative => global,
            BindingLocatorScope::Stack(index) => self
                .get(index as usize)
                .and_then(Environment::as_declarative)
                .expect("must be declarative environment"),
        };
        if env.get(binding_index).is_none() {
            env.set(binding_index, value);
        }
    }

    /// Push a private environment to the private environment stack.
    pub(crate) fn push_private(&mut self, environment: Gc<PrivateEnvironment>) {
        self.private_stack.push(environment);
    }

    /// Pop a private environment from the private environment stack.
    pub(crate) fn pop_private(&mut self) {
        self.private_stack.pop();
    }

    /// `ResolvePrivateIdentifier ( privEnv, identifier )`
    ///
    /// More information:
    ///  - [ECMAScript specification][spec]
    ///
    /// [spec]: https://tc39.es/ecma262/#sec-resolve-private-identifier
    pub(crate) fn resolve_private_identifier(&self, identifier: JsString) -> Option<PrivateName> {
        for environment in self.private_stack.iter().rev() {
            if environment.descriptions().contains(&identifier) {
                return Some(PrivateName::new(identifier, environment.id()));
            }
        }
        None
    }

    /// Return all private name descriptions in all private environments.
    pub(crate) fn private_name_descriptions(&self) -> Vec<&JsString> {
        let mut names = Vec::new();
        for environment in self.private_stack.iter().rev() {
            for name in environment.descriptions() {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
        names
    }

    /// Indicate if the current environment stack has an object environment.
    pub(crate) fn has_object_environment(&self) -> bool {
        self.iter_from_tip()
            .any(|(env, _)| matches!(env, Environment::Object(_)))
    }

    /// Create an `EnvironmentStack` snapshot suitable for storing in a closure.
    ///
    /// With the linked-list implementation, this is just a clone since
    /// cloning is O(1) — a single ref-count bump on the tip pointer.
    pub(crate) fn snapshot_for_closure(&mut self) -> EnvironmentStack {
        // Freeze the environments pushed since the last snapshot into the
        // shared chain (the closure and this frame then share them).
        for env in std::mem::take(&mut self.local) {
            self.tip = Some(Gc::new(EnvironmentNode { env, parent: self.tip.take() }));
        }
        self.clone()
    }

    // ---- Private helpers ----

    /// Push an environment onto the chain.
    fn push_env(&mut self, env: Environment) {
        self.local.push(env);
        self.depth += 1;
    }

    /// Compute the `(poisoned, with)` flags for a new environment.
    fn compute_poisoned_with(&self, global: &Gc<DeclarativeEnvironment>) -> (bool, bool) {
        let with = if let Some(env) = self.last() {
            env.as_declarative().is_none()
        } else {
            false
        };

        let environment = self
            .iter_from_tip()
            .find_map(|(env, _)| env.as_declarative())
            .unwrap_or(global);
        (environment.poisoned(), with || environment.with())
    }
}

/// Iterator from tip (most recent) toward root (oldest).
/// Yields `(&Environment, absolute_index)`.
struct EnvironmentChainIter<'a> {
    /// Unfrozen environments not yet yielded (yielded from the end).
    local: &'a [Environment],
    current: Option<&'a EnvironmentNode>,
    index: u32,
}

impl<'a> Iterator for EnvironmentChainIter<'a> {
    type Item = (&'a Environment, u32);

    fn next(&mut self) -> Option<Self::Item> {
        if let Some((last, rest)) = self.local.split_last() {
            self.local = rest;
            self.index -= 1;
            return Some((last, self.index));
        }
        let node = self.current?;
        self.index = self
            .index
            .checked_sub(1)
            .expect("iterator advanced past root");
        self.current = node.parent.as_deref();
        Some((&node.env, self.index))
    }
}

impl Context {
    /// Gets the corresponding runtime binding of the provided `BindingLocator`, modifying
    /// its indexes in place.
    ///
    /// This readjusts a `BindingLocator` to the correct binding if a `with` environment or
    /// `eval` call modified the compile-time bindings.
    ///
    /// Only use if the binding origin is unknown or comes from a `var` declaration. Lexical bindings
    /// are completely removed of runtime checks because the specification guarantees that runtime
    /// semantics cannot add or remove lexical bindings.
    pub(crate) fn find_runtime_binding(&mut self, locator: &mut BindingLocator) -> JsResult<()> {
        // OHB patch: a name that was unresolved when this code was compiled
        // (so it points at the global object) may since have been declared
        // with let/const/class by a later script; global lexical bindings
        // shadow global object properties (GetIdentifierReference walks the
        // global declarative record first).
        if matches!(locator.scope(), BindingLocatorScope::GlobalObject) {
            let frame = self.vm.frame();
            let realm_scope = frame.realm.scope();
            if realm_scope.num_bindings() > frame.code_block.global_scope_len
                && let Some(b) = realm_scope.get_binding(locator.name())
            {
                locator.set_scope(b.scope());
                locator.set_binding_index(b.binding_index());
            }
        }
        let deleted_binding = self.is_deleted_binding(locator);

        let global = self.vm.frame().realm.environment();
        if let Some(env) = self.vm.frame().environments.current_declarative_ref(global)
            && !env.with()
            && !env.poisoned()
            && !deleted_binding
        {
            return Ok(());
        }

        let (global, min_index) = match locator.scope() {
            BindingLocatorScope::GlobalObject | BindingLocatorScope::GlobalDeclarative => (true, 0),
            BindingLocatorScope::Stack(_) if deleted_binding => (false, 0),
            BindingLocatorScope::Stack(index) => (false, index),
        };
        let max_index = self.vm.frame().environments.len() as u32;

        for index in (min_index..max_index).rev() {
            match self.environment_expect(index) {
                Environment::Declarative(env) => {
                    if env.poisoned() || deleted_binding {
                        if let Some(env) = env.kind().as_function()
                            && let Some(b) = env.compile().get_binding(locator.name())
                        {
                            if env.is_deleted_binding(b.binding_index()) {
                                continue;
                            }
                            // OHB patch: a `var` that a direct eval added to the
                            // function's (shared) scope in an earlier call doesn't
                            // exist in this call until the eval runs again.
                            if !env.slot_initialized(b.binding_index()) {
                                continue;
                            }
                            locator.set_scope(b.scope());
                            locator.set_binding_index(b.binding_index());
                            return Ok(());
                        }
                    } else if !env.with() {
                        return Ok(());
                    }
                }
                Environment::Object(o) => {
                    let o = o.clone();
                    let key = locator.name().clone();
                    if o.has_property(key.clone(), self)? {
                        if let Some(unscopables) = o.get(JsSymbol::unscopables(), self)?.as_object()
                            && unscopables.get(key.clone(), self)?.to_boolean()
                        {
                            continue;
                        }
                        locator.set_scope(BindingLocatorScope::Stack(index));
                        return Ok(());
                    }
                }
            }
        }

        if deleted_binding
            && let BindingLocatorScope::Stack(index) = locator.scope()
            && let Environment::Declarative(env) = self.environment_expect(index)
            && let Some(function_env) = env.kind().as_function()
        {
            let mut scope = function_env.compile().outer();
            while let Some(current_scope) = scope {
                if let Some(binding) = current_scope.get_binding(locator.name()) {
                    locator.set_scope(binding.scope());
                    locator.set_binding_index(binding.binding_index());
                    return Ok(());
                }
                scope = current_scope.outer();
            }

            locator.set_scope(BindingLocatorScope::GlobalObject);
            locator.set_binding_index(0);
            return Ok(());
        }

        if global
            && self.realm().environment().poisoned()
            && let Some(b) = self.realm().scope().get_binding(locator.name())
        {
            locator.set_scope(b.scope());
            locator.set_binding_index(b.binding_index());
        }

        Ok(())
    }

    /// Finds the object environment that contains the binding and returns the `this` value of the object environment.
    pub(crate) fn this_from_object_environment_binding(
        &mut self,
        locator: &BindingLocator,
    ) -> JsResult<Option<JsObject>> {
        let global = self.vm.frame().realm.environment();
        if let Some(env) = self.vm.frame().environments.current_declarative_ref(global)
            && !env.with()
        {
            return Ok(None);
        }

        let min_index = match locator.scope() {
            BindingLocatorScope::GlobalObject | BindingLocatorScope::GlobalDeclarative => 0,
            BindingLocatorScope::Stack(index) => index,
        };
        let max_index = self.vm.frame().environments.len() as u32;

        for index in (min_index..max_index).rev() {
            match self.environment_expect(index) {
                Environment::Declarative(env) => {
                    if env.poisoned() {
                        if let Some(env) = env.kind().as_function()
                            && env.compile().get_binding(locator.name()).is_some()
                        {
                            break;
                        }
                    } else if !env.with() {
                        break;
                    }
                }
                Environment::Object(o) => {
                    let o = o.clone();
                    let key = locator.name().clone();
                    if o.has_property(key.clone(), self)? {
                        if let Some(unscopables) = o.get(JsSymbol::unscopables(), self)?.as_object()
                            && unscopables.get(key.clone(), self)?.to_boolean()
                        {
                            continue;
                        }
                        return Ok(Some(o));
                    }
                }
            }
        }

        Ok(None)
    }

    /// Checks if the binding pointed by `locator` is initialized.
    ///
    /// # Panics
    ///
    /// Panics if the environment or binding index are out of range.
    pub(crate) fn is_initialized_binding(&mut self, locator: &BindingLocator) -> JsResult<bool> {
        match locator.scope() {
            BindingLocatorScope::GlobalObject => {
                let key = locator.name().clone();
                let obj = self.global_object();
                obj.has_property(key, self)
            }
            BindingLocatorScope::GlobalDeclarative => {
                let env = self.vm.frame().realm.environment();
                Ok(env.get(locator.binding_index()).is_some())
            }
            BindingLocatorScope::Stack(index) => match self.environment_expect(index) {
                Environment::Declarative(env) => Ok(env.get(locator.binding_index()).is_some()),
                Environment::Object(obj) => {
                    let key = locator.name().clone();
                    let obj = obj.clone();
                    obj.has_property(key, self)
                }
            },
        }
    }

    /// OHB fast path: the declarative environment holding `locator`'s binding
    /// when no runtime adjustment applies (the case `find_runtime_binding`
    /// returns early for: no `with`, no poisoning by direct eval, binding not
    /// deleted). None: take the general path.
    #[inline]
    pub(crate) fn fast_stack_env(&self, locator: &BindingLocator) -> Option<&Gc<DeclarativeEnvironment>> {
        let BindingLocatorScope::Stack(index) = locator.scope() else { return None };
        let frame = self.vm.frame();
        let cur = frame.environments.current_declarative_ref(frame.realm.environment())?;
        if cur.with() || cur.poisoned() {
            return None;
        }
        match frame.environments.get(index as usize)? {
            Environment::Declarative(env) if !env.is_deleted_binding(locator.binding_index()) => Some(env),
            _ => None,
        }
    }

    pub(crate) fn is_deleted_binding(&self, locator: &BindingLocator) -> bool {
        match locator.scope() {
            BindingLocatorScope::Stack(index) => matches!(
                self.environment_expect(index),
                Environment::Declarative(env) if env.is_deleted_binding(locator.binding_index())
            ),
            BindingLocatorScope::GlobalObject | BindingLocatorScope::GlobalDeclarative => false,
        }
    }

    pub(crate) fn restore_deleted_binding(&self, locator: &BindingLocator) {
        if let BindingLocatorScope::Stack(index) = locator.scope()
            && let Environment::Declarative(env) = self.environment_expect(index)
        {
            env.restore_deleted_binding(locator.binding_index());
        }
    }

    /// Get the value of a binding.
    ///
    /// # Panics
    ///
    /// Panics if the environment or binding index are out of range.
    #[track_caller]
    pub(crate) fn get_binding(&mut self, locator: &BindingLocator) -> JsResult<Option<JsValue>> {
        match locator.scope() {
            BindingLocatorScope::GlobalObject => {
                let key = locator.name().clone();
                let obj = self.global_object();
                obj.try_get(key, self)
            }
            BindingLocatorScope::GlobalDeclarative => {
                let env = self.vm.frame().realm.environment();
                Ok(env.get(locator.binding_index()))
            }
            BindingLocatorScope::Stack(index) => match self.environment_expect(index) {
                Environment::Declarative(env) => {
                    let v = env.get(locator.binding_index());
                    // OHB patch: a `var` added to a function's scope by a direct
                    // eval in an earlier call, and not (yet) declared in this
                    // one: the name resolves further out (here: the global).
                    if v.is_none()
                        && env.poisoned()
                        && let Some(f) = env.kind().as_function()
                        && !f.slot_initialized(locator.binding_index())
                    {
                        let name = locator.name().clone();
                        let realm_env = self.vm.frame().realm.environment().clone();
                        if let Some(b) = self.vm.frame().realm.scope().get_binding(&name) {
                            return Ok(realm_env.get(b.binding_index()));
                        }
                        let obj = self.global_object();
                        if obj.has_property(name.clone(), self)? {
                            return obj.get(name, self).map(Some);
                        }
                    }
                    Ok(v)
                }
                Environment::Object(obj) => {
                    let key = locator.name().clone();
                    let obj = obj.clone();
                    obj.get(key, self).map(Some)
                }
            },
        }
    }

    /// Sets the value of a binding.
    ///
    /// # Panics
    ///
    /// Panics if the environment or binding index are out of range.
    #[track_caller]
    pub(crate) fn set_binding(
        &mut self,
        locator: &BindingLocator,
        value: JsValue,
        strict: bool,
    ) -> JsResult<()> {
        match locator.scope() {
            BindingLocatorScope::GlobalObject => {
                let key = locator.name().clone();
                let obj = self.global_object();
                obj.set(key, value, strict, self)?;
            }
            BindingLocatorScope::GlobalDeclarative => {
                let env = self.vm.frame().realm.environment();
                env.set(locator.binding_index(), value);
            }
            BindingLocatorScope::Stack(index) => match self.environment_expect(index) {
                Environment::Declarative(decl) => {
                    decl.set(locator.binding_index(), value);
                }
                Environment::Object(obj) => {
                    let key = locator.name().clone();
                    let obj = obj.clone();
                    obj.set(key, value, strict, self)?;
                }
            },
        }
        Ok(())
    }

    /// Deletes a binding if it exists.
    ///
    /// Returns `true` if the binding was deleted.
    ///
    /// # Panics
    ///
    /// Panics if the environment or binding index are out of range.
    pub(crate) fn delete_binding(&mut self, locator: &BindingLocator) -> JsResult<bool> {
        match locator.scope() {
            BindingLocatorScope::GlobalObject => {
                let key = locator.name().clone();
                let obj = self.global_object();
                obj.__delete__(&key.into(), &mut self.into())
            }
            BindingLocatorScope::GlobalDeclarative => Ok(false),
            BindingLocatorScope::Stack(index) => match self.environment_expect(index) {
                Environment::Declarative(env) => Ok(env.delete_binding(locator.binding_index())),
                Environment::Object(obj) => {
                    let key = locator.name().clone();
                    let obj = obj.clone();
                    obj.__delete__(&key.into(), &mut self.into())
                }
            },
        }
    }

    /// Return the environment at the given index.
    ///
    /// # Panics
    ///
    /// Panics if the `index` is out of range.
    pub(crate) fn environment_expect(&self, index: u32) -> &Environment {
        self.vm
            .frame()
            .environments
            .get(index as usize)
            .expect("environment index must be in range")
    }
}
