//! `environment` holds the state of the compiler.

use anyhow::{bail, Result};
pub use inkwell::context::Context;
use inkwell::{
    basic_block::BasicBlock,
    builder::Builder,
    module::{Linkage, Module},
    types::{BasicTypeEnum, FunctionType},
    values::{BasicValueEnum, FunctionValue, GlobalValue, IntValue},
    AddressSpace,
};
use std::path::Path;
use wasmparser::MemArg;

use crate::inkwell::{init_inkwell, InkwellInsts, InkwellTypes};
use crate::insts::control::{ControlFrame, UnreachableReason};

pub enum Global<'a> {
    Mut {
        ptr_to_value: GlobalValue<'a>,
        ty: BasicTypeEnum<'a>,
    },
    Const {
        value: BasicValueEnum<'a>,
    },
}

pub struct Environment<'a, 'b> {
    // Output dir
    pub output_file: Option<&'b Path>,

    // Inkwell code generator
    pub context: &'a Context,
    pub module: Module<'a>,
    pub builder: Builder<'a>,

    // Set of primitive types of inkwell
    pub inkwell_types: InkwellTypes<'a>,
    pub inkwell_insts: InkwellInsts<'a>,

    // List of all signatures
    pub function_signature_list: Vec<FunctionType<'a>>,

    // List of functions
    pub function_list: Vec<FunctionValue<'a>>,
    pub function_list_signature: Vec<u32>,
    pub function_list_name: Vec<String>,

    // Stack for Wasm binary
    pub stack: Vec<BasicValueEnum<'a>>,

    // Global variables
    pub global: Vec<Global<'a>>,

    pub import_section_size: u32,
    pub function_section_size: u32,

    pub current_function_idx: u32,

    // ControlFrame
    pub control_frames: Vec<ControlFrame<'a>>,

    pub wasker_init_block: Option<BasicBlock<'a>>,
    pub wasker_main_block: Option<BasicBlock<'a>>,

    pub start_function_idx: Option<u32>,

    pub unreachable_depth: u32,
    pub unreachable_reason: UnreachableReason,

    // Table
    pub global_table: Option<GlobalValue<'a>>,

    pub memory_manager: Option<MemoryManager<'a>>,
}

pub struct MemoryManager<'env> {
    /// An external function that returns the base address of the memory.
    pub fn_memory_base: FunctionValue<'env>,
    /// An external function that grows the memory by a given wasm page size.
    pub fn_memory_grow: FunctionValue<'env>,
    /// An external function that grows the memory by a given page size.
    pub fn_memory_error_handler: FunctionValue<'env>,

    pub fn_memory_bound_checker: FunctionValue<'env>,

    /// A global variable that caches meta data of the Wasm memory.
    /// It can only be modified after the host call `memory_grow`.
    pub global_memory: GlobalMemoryMeta<'env>,
}

impl<'env> MemoryManager<'env> {
    pub fn init_within(
        context: &'env Context,
        module: &Module<'env>,
        builder: &Builder<'env>,
        types: &InkwellTypes<'env>,
        init_mem_size: u64,
    ) -> Self {
        // Define external memory_base OS Call in module
        let wasm_mem_base_type = GlobalMemoryMeta::wasm_mem_base_addr_type(types);
        let fn_type_memory_base = wasm_mem_base_type.fn_type(&[], false);
        let fn_memory_base = module.add_function("memory_base", fn_type_memory_base, None);

        // Define external memory_grow OS Call in module
        let wasm_mem_page_size_type = GlobalMemoryMeta::wasm_mem_page_size_type(types);
        let fn_type_memory_grow =
            wasm_mem_page_size_type.fn_type(&[wasm_mem_page_size_type.into()], false);
        let fn_memory_grow = module.add_function("memory_grow", fn_type_memory_grow, None);

        // Define external memory_error_handler OS Call in module
        let wasm_mem_errno_type = GlobalMemoryMeta::wasm_mem_errno_type(types);
        let fn_type_memory_error_handler = types
            .void_type
            .fn_type(&[wasm_mem_errno_type.into()], false);
        let fn_memory_error_handler =
            module.add_function("memory_error_handler", fn_type_memory_error_handler, None);

        let global_memory = GlobalMemoryMeta::init_within(module, types);
        // malloc the initial memory from OS
        let page_size_int_val = wasm_mem_page_size_type.const_int(init_mem_size, false);
        Self::_build_call_fn_memory_grow(builder, fn_memory_grow, page_size_int_val);
        global_memory.store_size(builder, page_size_int_val);
        global_memory.fetch_and_store_base_addr(builder, fn_memory_base);

        // [wasm_page_size, mem_offset_in_bytes] -> void
        let mem_bnd_checker_ty = types.void_type.fn_type(&[wasm_mem_page_size_type.into(), types.i64_type.into()], false);
        let fn_memory_bound_checker = module.add_function(
            "memory_bound_checker",
            mem_bnd_checker_ty,
            None);

        // let then_block = context.append_basic_block(fn_memory_bound_checker, "then");
        // let merge_block = context.append_basic_block(fn_memory_bound_checker,"merge");




        Self {
            fn_memory_base,
            fn_memory_grow,
            fn_memory_error_handler,
            fn_memory_bound_checker,
            global_memory,
        }
    }

    fn _build_call_fn_memory_grow(
        builder: &Builder<'env>,
        fn_memory_grow: FunctionValue<'env>,
        page_size_delta: IntValue<'env>,
    ) -> IntValue<'env> {
        builder
            .build_call(
                fn_memory_grow,
                &[page_size_delta.into()],
                "grow_linear_memory",
            )
            .try_as_basic_value()
            .left()
            .expect("error build_call memory_grow")
            .into_int_value()
    }

    /// Grows the memory by a given page size delta through the external call.
    /// Updates the global memory size and base address.
    /// Returns the new page size.
    pub fn build_call_fn_memory_grow(
        &self,
        builder: &Builder<'env>,
        page_size_delta: IntValue<'env>,
    ) -> IntValue<'env> {
        let old_page_size =
            Self::_build_call_fn_memory_grow(builder, self.fn_memory_grow, page_size_delta);
        self.global_memory
            .fetch_and_store_base_addr(builder, self.fn_memory_base);
        let new_page_size = builder.build_int_add(old_page_size, page_size_delta, "new_page_size");
        self.global_memory.store_size(builder, new_page_size);
        new_page_size
    }

    pub fn check_mem<'b>(&self, memarg: &MemArg, env: &Environment<'env, 'b>) {
        let memarg_offset = env.inkwell_types.i64_type.const_int(memarg.offset, false);

        let wasm_page_size = self
            .global_memory
            .load_size(&env.builder, &env.inkwell_types)
            .into_int_value();
        let max_offet = env.builder.build_int_mul(
            wasm_page_size,
            env.inkwell_types.i64_type.const_int(1u64 << 16, false),
            "max_offset",
        );

        // let cond = env.builder.build_int_compare(
        //     inkwell::IntPredicate::UGE,
        //     memarg_offset,
        //     max_offet,
        //     "out of boundary",
        // );

        // let tmp_fn_ty = env.inkwell_types.void_type.fn_type(&[], false);
        // let tmp_fn = env
        //     .module
        //     .add_function("tmp_fn", tmp_fn_ty, Some(Linkage::Common));

        // let then_block = env
        //     .context
        //     .append_basic_block(env.function_list[env.current_function_idx as usize], "then");
        // let merge_block = env.context.append_basic_block(
        //     env.function_list[env.current_function_idx as usize],
        //     "merge",
        // );

        // env.builder
        //     .build_conditional_branch(cond, then_block, merge_block);
        // env.builder.position_at_end(then_block);
        // let mem_err_type = GlobalMemoryMeta::wasm_mem_errno_type(&env.inkwell_types);
        // env.builder.build_call(
        //     self.fn_memory_error_handler,
        //     &[mem_err_type.const_zero().into()],
        //     "out of boundary handler",
        // );
        // // env.builder.build_unreachable();
        // env.builder.build_unconditional_branch(merge_block);
        // env.builder.position_at_end(merge_block);
    }
}

pub struct GlobalMemoryMeta<'env> {
    /// The size of the memory in pages.
    size: GlobalValue<'env>,
    /// The base address of the memory.
    base_addr: GlobalValue<'env>,
}
impl<'env> GlobalMemoryMeta<'env> {
    /// Creates global variables that cache the meta data of the Wasm memory.
    ///
    /// init_mem_size: The initial total size of the memory in pages
    /// from the memory section in Wasm binaries.
    pub fn init_within(module: &Module<'env>, types: &InkwellTypes<'env>) -> Self {
        let wasm_mem_page_size_type = Self::wasm_mem_page_size_type(types);
        let size = module.add_global(
            wasm_mem_page_size_type,
            Some(AddressSpace::default()),
            "global_memory_size",
        );
        size.set_initializer(&wasm_mem_page_size_type.const_zero());
        // let size_value = wasm_mem_page_size_type.const_zero();

        let wasm_mem_base_addr_type = Self::wasm_mem_base_addr_type(types);
        let base_addr = module.add_global(
            wasm_mem_base_addr_type,
            Some(AddressSpace::default()),
            "global_memory_base_addr",
        );
        base_addr.set_initializer(&wasm_mem_base_addr_type.const_zero());

        Self { size, base_addr }
    }

    const fn wasm_mem_page_size_type(types: &InkwellTypes<'env>) -> inkwell::types::IntType<'env> {
        types.i32_type
    }
    const fn wasm_mem_base_addr_type(
        types: &InkwellTypes<'env>,
    ) -> inkwell::types::PointerType<'env> {
        types.i8_ptr_type
    }
    const fn wasm_mem_errno_type(types: &InkwellTypes<'env>) -> inkwell::types::IntType<'env> {
        types.i8_type
    }

    /// Updates new memory page size
    fn store_size(&self, builder: &Builder<'env>, page_size: IntValue<'env>) {
        builder.build_store(self.size.as_pointer_value(), page_size);
    }
    /// Loads memory page size
    pub fn load_size(
        &self,
        builder: &Builder<'env>,
        types: &InkwellTypes<'env>,
    ) -> BasicValueEnum<'env> {
        let size_type = Self::wasm_mem_page_size_type(types);
        builder.build_load(size_type, self.size.as_pointer_value(), "mem_size")
    }

    fn fetch_and_store_base_addr(
        &self,
        builder: &Builder<'env>,
        fn_memory_base: FunctionValue<'env>,
    ) {
        let linear_memory_base_ptr = builder
            .build_call(fn_memory_base, &[], "linear_memory_base_int")
            .try_as_basic_value()
            .left()
            .expect("error build_call memory_base");

        builder.build_store(self.base_addr.as_pointer_value(), linear_memory_base_ptr);
    }
    pub fn load_base_addr(
        &self,
        builder: &Builder<'env>,
        types: &InkwellTypes<'env>,
    ) -> BasicValueEnum<'env> {
        let base_addr_type = Self::wasm_mem_base_addr_type(types);
        builder.build_load(
            base_addr_type,
            self.base_addr.as_pointer_value(),
            "mem_base",
        )
    }
}

impl<'a, 'b> Environment<'a, 'b> {
    pub fn new(context: &'a Context) -> Self {
        let module = context.create_module("wasker_module");
        let builder = context.create_builder();
        let (inkwell_types, inkwell_insts) = init_inkwell(context, &module);

        Self {
            output_file: None,
            context,
            module,
            builder,
            inkwell_types,
            inkwell_insts,
            function_signature_list: Vec::new(),
            function_list: Vec::new(),
            function_list_signature: Vec::new(),
            function_list_name: Vec::new(),
            stack: Vec::new(),
            global: Vec::new(),
            import_section_size: 0,
            function_section_size: 0,
            current_function_idx: u32::MAX,
            control_frames: Vec::new(),
            wasker_init_block: None,
            wasker_main_block: None,
            start_function_idx: None,
            unreachable_depth: 0,
            unreachable_reason: UnreachableReason::Reachable,
            global_table: None,
            memory_manager: None,
        }
    }

    pub fn output_file(&mut self, output_file: &'b Path) {
        self.output_file.replace(output_file);
    }

    /// Restore the stack to the specified size.
    pub fn reset_stack(&mut self, stack_size: usize) {
        self.stack.truncate(stack_size);
    }

    /// Pop the stack and load the value if it is a pointer.
    pub fn pop_and_load(&mut self) -> BasicValueEnum<'a> {
        let pop = self.stack.pop().expect("stack empty");
        if pop.is_pointer_value() {
            self.builder.build_load(
                self.inkwell_types.i64_type,
                pop.into_pointer_value(),
                "from_stack",
            )
        } else {
            pop
        }
    }

    /// Get the control frame immediately outside the current control frame.
    pub fn ref_outer_frame(&self) -> &ControlFrame<'a> {
        let frame_len = self.control_frames.len();
        assert_ne!(frame_len, 0);
        &self.control_frames[frame_len - 1]
    }

    /// Pop two values from the stack.
    pub fn pop2(&mut self) -> (BasicValueEnum<'a>, BasicValueEnum<'a>) {
        let v2 = self.stack.pop().expect("stack empty");
        let v1 = self.stack.pop().expect("stack empty");
        (v1, v2)
    }

    /// Peek values from the stack.
    pub fn peekn(&self, n: usize) -> Result<&[BasicValueEnum<'a>]> {
        if self.stack.len() < n {
            bail!("stack length too short {} vs {}", self.stack.len(), n);
        }
        let index = self.stack.len() - n;
        Ok(&self.stack[index..])
    }
}
