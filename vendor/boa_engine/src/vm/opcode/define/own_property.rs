use crate::{
    Context, JsNativeError, JsResult,
    object::internal_methods::InternalMethodPropertyContext,
    property::PropertyDescriptor,
    vm::opcode::{IndexOperand, Operation, RegisterOperand},
};

/// Whether `obj` defines and reads own properties the ordinary way.
#[inline]
pub(crate) fn is_plain_define(obj: &crate::JsObject) -> bool {
    use crate::object::internal_methods::{ordinary_define_own_property, ordinary_get_own_property};
    let vt = obj.vtable();
    vt.__define_own_property__ as usize == ordinary_define_own_property as usize && vt.__get_own_property__ as usize == ordinary_get_own_property as usize
}

/// `DefineOwnPropertyByName` implements the Opcode Operation for `Opcode::DefineOwnPropertyByName`
///
/// Operation:
///  - Defines a own property of an object by name.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DefineOwnPropertyByName;

impl DefineOwnPropertyByName {
    #[inline(always)]
    pub(crate) fn operation(
        (object, value, index): (RegisterOperand, RegisterOperand, IndexOperand),
        context: &mut Context,
    ) -> JsResult<()> {
        let object = context.vm.get_register(object.into()).clone();
        let value = context.vm.get_register(value.into()).clone();
        let name = context
            .vm
            .frame()
            .code_block()
            .constant_string(index.into());
        let object = object.to_object(context)?;
        let key: crate::property::PropertyKey = name.into();
        // OHB patch: ordinary extensible objects (object literals) take a
        // direct shape transition.
        let value = if is_plain_define(&object) {
            let mut o = object.borrow_mut();
            if o.extensible {
                match o.properties.fast_define_data_wec(&key, value) {
                    Ok(()) => return Ok(()),
                    Err(v) => v,
                }
            } else {
                value
            }
        } else {
            value
        };
        object.__define_own_property__(
            &key,
            PropertyDescriptor::builder()
                .value(value.clone())
                .writable(true)
                .enumerable(true)
                .configurable(true)
                .build(),
            &mut InternalMethodPropertyContext::new(context),
        )?;
        Ok(())
    }
}

impl Operation for DefineOwnPropertyByName {
    const NAME: &'static str = "DefineOwnPropertyByName";
    const INSTRUCTION: &'static str = "INST - DefineOwnPropertyByName";
    const COST: u8 = 4;
}

/// `DefineOwnPropertyByValue` implements the Opcode Operation for `Opcode::DefineOwnPropertyByValue`
///
/// Operation:
///  - Defines a own property of an object by value.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DefineOwnPropertyByValue;

impl DefineOwnPropertyByValue {
    #[inline(always)]
    pub(crate) fn operation(
        (value, key, object): (RegisterOperand, RegisterOperand, RegisterOperand),
        context: &mut Context,
    ) -> JsResult<()> {
        let value = context.vm.get_register(value.into()).clone();
        let key = context.vm.get_register(key.into()).clone();
        let object = context.vm.get_register(object.into()).clone();
        let object = object.to_object(context)?;
        let key = key.to_property_key(context)?;
        let success = object.__define_own_property__(
            &key,
            PropertyDescriptor::builder()
                .value(value.clone())
                .writable(true)
                .enumerable(true)
                .configurable(true)
                .build(),
            &mut InternalMethodPropertyContext::new(context),
        )?;
        if !success {
            return Err(JsNativeError::typ()
                .with_message("failed to defined own property")
                .into());
        }
        Ok(())
    }
}

impl Operation for DefineOwnPropertyByValue {
    const NAME: &'static str = "DefineOwnPropertyByValue";
    const INSTRUCTION: &'static str = "INST - DefineOwnPropertyByValue";
    const COST: u8 = 4;
}
