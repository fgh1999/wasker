//! Definition of memory instructions.

use crate::environment::Environment;
use anyhow::{anyhow, Context, Ok, Result};
use inkwell::{
    types::{BasicType, PointerType},
    values::{BasicValue, IntValue, PointerValue},
    AddressSpace,
};
use wasmparser::MemArg;

pub fn memory_size(environment: &mut Environment<'_, '_>) -> Result<()> {
    let size = environment
        .memory_manager
        .as_ref()
        .expect("should define memory_manager")
        .global_memory
        .load_size(&environment.builder, &environment.inkwell_types);
    environment.stack.push(size);
    Ok(())
}

pub fn memory_grow(environment: &mut Environment<'_, '_>) -> Result<()> {
    // Request to OS
    let delta = environment.stack.pop().expect("stack empty");
    let page_size_delta = delta.into_int_value();

    let mem_mgr = &environment
        .memory_manager
        .as_ref()
        .expect("should define memory_manager");
    // Load old memory size
    let old_size = mem_mgr
        .global_memory
        .load_size(&environment.builder, &environment.inkwell_types);
    mem_mgr.build_call_fn_memory_grow(&environment.builder, page_size_delta);
    environment.stack.push(old_size);

    Ok(())
}

pub fn memory_copy(
    environment: &mut Environment<'_, '_>,
    dst_mem: u32,
    src_mem: u32,
) -> Result<()> {
    // TODO: multi memory
    assert_eq!(dst_mem, 0);
    assert_eq!(src_mem, 0);

    let len = environment.stack.pop().expect("stack empty");
    let src = environment.stack.pop().expect("stack empty");
    let dst = environment.stack.pop().expect("stack empty");
    let src_addr = resolve_pointer(
        src.into_int_value(),
        environment
            .inkwell_types
            .i32_type
            .ptr_type(AddressSpace::default()),
        environment,
    );
    let dst_addr = resolve_pointer(
        dst.into_int_value(),
        environment
            .inkwell_types
            .i32_type
            .ptr_type(AddressSpace::default()),
        environment,
    );
    environment
        .builder
        .build_memcpy(dst_addr, 1, src_addr, 1, len.into_int_value())
        .map_err(|e| anyhow!(e))
        .context("error build_memcpy")?;
    Ok(())
}

pub fn memory_fill(environment: &mut Environment<'_, '_>, mem: u32) -> Result<()> {
    // TODO: multi memory
    assert_eq!(mem, 0);

    let len = environment.stack.pop().expect("stack empty");
    let val = environment.stack.pop().expect("stack empty");
    let dst = environment.stack.pop().expect("stack empty");
    let dst_addr = resolve_pointer(
        dst.into_int_value(),
        environment
            .inkwell_types
            .i32_type
            .ptr_type(AddressSpace::default()),
        environment,
    );
    let val_i8 = environment.builder.build_int_truncate(
        val.into_int_value(),
        environment.inkwell_types.i8_type,
        "val_i8",
    );
    environment
        .builder
        .build_memset(dst_addr, 1, val_i8, len.into_int_value())
        .map_err(|e| anyhow!(e))
        .context("error build_memset")?;
    Ok(())
}

// generate IR for load instructions
pub fn generate_load<'a>(
    memarg: &MemArg,
    extended_type: inkwell::types::BasicTypeEnum<'a>,
    load_type: inkwell::types::BasicTypeEnum<'a>,
    signed: bool,
    require_extend: bool,
    environment: &mut Environment<'a, '_>,
) -> Result<()> {
    // environment
    //     .memory_manager
    //     .as_ref()
    //     .expect("should define memory_manager")
    //     .check_mem(memarg, environment);

    // offset
    let address_operand = environment
        .stack
        .pop()
        .expect("stack empty")
        .into_int_value();
    let address_operand_ex = environment.builder.build_int_z_extend(
        address_operand,
        environment.inkwell_types.i64_type,
        "",
    );
    let memarg_offset = environment
        .inkwell_types
        .i64_type
        .const_int(memarg.offset, false);
    let offset = environment
        .builder
        .build_int_add(address_operand_ex, memarg_offset, "offset");

    // get actual virtual address
    let dst_addr = resolve_pointer(
        offset,
        load_type.ptr_type(AddressSpace::default()),
        environment,
    );
    // load value
    let result = environment
        .builder
        .build_load(load_type, dst_addr, "loaded");

    // push loaded value
    if require_extend {
        // extend value
        let extended_result = match signed {
            true => environment.builder.build_int_s_extend(
                result.into_int_value(),
                extended_type.into_int_type(),
                "loaded_extended",
            ),
            false => environment.builder.build_int_z_extend(
                result.into_int_value(),
                extended_type.into_int_type(),
                "loaded_extended",
            ),
        };
        environment
            .stack
            .push(extended_result.as_basic_value_enum());
    } else {
        environment.stack.push(result.as_basic_value_enum());
    }
    Ok(())
}

// generate IR for store instructions
// see generate_load
pub fn generate_store<'a>(
    memarg: &MemArg,
    store_type: inkwell::types::BasicTypeEnum<'a>,
    require_narrow: bool,
    environment: &mut Environment<'a, '_>,
) -> Result<()> {
    // environment
    //     .memory_manager
    //     .as_ref()
    //     .expect("should define memory_manager")
    //     .check_mem(memarg, environment);

    // value
    let value = environment.stack.pop().expect("stack empty");

    // offset
    let address_operand = environment
        .stack
        .pop()
        .expect("stack empty")
        .into_int_value();
    let address_operand_ex = environment.builder.build_int_z_extend(
        address_operand,
        environment.inkwell_types.i64_type,
        "",
    );
    let memarg_offset = environment
        .inkwell_types
        .i64_type
        .const_int(memarg.offset, false);
    let offset = environment
        .builder
        .build_int_add(address_operand_ex, memarg_offset, "offset");

    // get actual virtual address
    let dst_addr = resolve_pointer(
        offset,
        store_type.ptr_type(AddressSpace::default()),
        environment,
    );

    if require_narrow {
        let narrow_value = environment.builder.build_int_truncate(
            value.into_int_value(),
            store_type.into_int_type(),
            "narrow_value",
        );
        environment.builder.build_store(dst_addr, narrow_value);
    } else {
        environment.builder.build_store(dst_addr, value);
    }

    Ok(())
}

fn resolve_pointer<'a>(
    offset: IntValue<'a>,
    ptr_type: PointerType<'a>,
    environment: &mut Environment<'a, '_>,
) -> PointerValue<'a> {
    // get base addr of the current linear memory from OS
    let linear_memory_base_int = environment
        .memory_manager
        .as_ref()
        .expect("should define memory_manager")
        .global_memory
        .load_base_addr(&environment.builder, &environment.inkwell_types);

    // calculate base + offset
    let dst_addr = unsafe {
        environment.builder.build_gep(
            environment.inkwell_types.i8_type,
            linear_memory_base_int.into_pointer_value(),
            &[offset],
            "resolved_addr",
        )
    };
    // cast pointer value
    environment
        .builder
        .build_bitcast(dst_addr, ptr_type, "bit_casted")
        .into_pointer_value()
}
