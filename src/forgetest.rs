// Copyright (C) 2025 the DTVM authors. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

// example:
// TODO: yul2wasm forge-test not need to pass project-dir and out-dir
// yul2wasm forge-test --project-dir ./examples/foundry_erc20 --out-dir ./examples/foundry_erc20/out --verbose

use crate::yul2ir::config::Yul2IROptions;
use crate::yul2ir::context::Yul2IRContext;
use crate::yul2ir::utils;
use crate::yul2ir::yul;
use crate::yul2ir::yul_instruction::YulLowLevelValueType;
use crate::Args;
use clap::arg;
use clap::Parser;
use inkwell::context::Context;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{self, Command};

#[derive(Parser, Debug)]
pub struct ForgeTestArgs {
    /// Directory containing the project
    #[arg(
        long = "project-dir",
        help = "Directory containing the project",
        default_value = "."
    )]
    project_dir: String,

    /// Output directory for compiled tests
    #[arg(
        long = "out-dir",
        help = "Output directory for compiled tests",
        default_value = "out"
    )]
    out_dir: String,

    /// Test contract deploy address
    #[arg(
        long = "test-address",
        help = "Test contract deploy address",
        default_value = "0x11bbccddeeffaabbccddeeffaabbccddeeffaa11"
    )]
    test_address: String,

    /// WasmTestVM deploy address
    #[arg(
        long = "vm-address",
        help = "WasmTestVM deploy address",
        default_value = "0x7109709ecfa91a80626ff3989d68f67f5b1dd12d"
    )]
    vm_address: String,

    /// Verbose output
    #[arg(long = "verbose", help = "Verbose output")]
    verbose: bool,

    /// Debug mode
    #[arg(long = "debug", help = "Debug mode")]
    debug: bool,

    /// Build mode (debug or release)
    #[arg(long = "build-mode", help = "Build mode", default_value = "release")]
    build_mode: String,
}

/// Run forge tests for the project
pub fn run_forge_tests(forge_args: &ForgeTestArgs, args: &Args) {
    println!("Running forge tests in {} mode...", forge_args.build_mode);

    // Setup project directory
    let project_dir = PathBuf::from(&forge_args.project_dir);
    let out_dir = PathBuf::from(&forge_args.out_dir);

    // Create output directory if it doesn't exist
    if !out_dir.exists() {
        fs::create_dir_all(&out_dir).unwrap_or_else(|e| {
            eprintln!("Error creating output directory: {}", e);
            process::exit(1);
        });
    }

    // Step 1: Find WasmTestVM.sol and copy it to the src directory
    let wasmtestvm_path = find_wasmtestvm();
    let src_dir = project_dir.join("src");
    if !src_dir.exists() {
        fs::create_dir_all(&src_dir).unwrap_or_else(|e| {
            eprintln!("Error creating src directory: {}", e);
            process::exit(1);
        });
    }

    let target_vm_path = src_dir.join("WasmTestVM.sol");
    println!("Copying WasmTestVM.sol to {}", target_vm_path.display());
    fs::copy(wasmtestvm_path, &target_vm_path).unwrap_or_else(|e| {
        eprintln!("Error copying WasmTestVM.sol: {}", e);
        process::exit(1);
    });

    // Step 2: Run forge test to generate IR files
    println!("Running forge test to generate IR files...");
    let forge_status = Command::new("forge")
        .current_dir(&project_dir)
        .arg("test")
        .arg("--extra-output-files")
        .arg("ir-optimized")
        .status()
        .unwrap_or_else(|e| {
            eprintln!("Error running forge test: {}", e);
            process::exit(1);
        });

    if !forge_status.success() {
        eprintln!("Forge test failed");
        process::exit(1);
    }

    // Step 3: Find all contracts in the out directory
    let mut contracts = vec!["WasmTestVM".to_string()];
    let test_contracts = find_test_contracts(&out_dir);
    contracts.extend(test_contracts);

    println!("Found contracts to compile: {:?}", contracts);

    // Step 4: Compile all contracts
    for contract in &contracts {
        compile_contract(contract, &out_dir, args, forge_args.verbose);
    }

    // Step 5: Run the tests
    run_tests(
        &contracts,
        &out_dir,
        &forge_args.vm_address,
        &forge_args.test_address,
    );

    // Cleanup
    fs::remove_file(target_vm_path).ok();

    println!("All forge tests completed successfully!");
}

// TODO: put WasmTestVM logic in chain_mockcli
/// Find WasmTestVM.sol in common locations
fn find_wasmtestvm() -> PathBuf {
    let possible_paths = [
        "scripts/WasmTestVM.sol",
        "../scripts/WasmTestVM.sol",
        "examples/scripts/WasmTestVM.sol",
        "../examples/scripts/WasmTestVM.sol",
    ];

    for path in &possible_paths {
        let p = PathBuf::from(path);
        if p.exists() {
            return p;
        }
    }

    eprintln!("Error: Could not find WasmTestVM.sol");
    process::exit(1);
}

/// Find test contracts in the out directory
fn find_test_contracts(out_dir: &PathBuf) -> Vec<String> {
    let mut contracts = Vec::new();

    if let Ok(entries) = fs::read_dir(out_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                let file_name = path.file_name().unwrap().to_string_lossy().to_string();
                if file_name.ends_with(".sol") {
                    let contract_name = file_name.trim_end_matches(".sol").to_string();
                    if contract_name != "WasmTestVM"
                        && (contract_name.starts_with("Test") || contract_name.contains("Test"))
                    {
                        contracts.push(contract_name);
                    }
                }
            }
        }
    }

    contracts
}

/// Compile a contract to WASM
fn compile_contract(contract: &str, out_dir: &Path, args: &Args, verbose: bool) {
    println!("Compiling {}...", contract);

    let ir_path = out_dir.join(format!("{}.sol/{}.iropt", contract, contract));
    let wasm_path = out_dir.join(format!("{}.wasm", contract));

    if !ir_path.exists() {
        eprintln!("Error: IR file not found at {}", ir_path.display());
        process::exit(1);
    }

    // Create a new context and compile the contract
    let yul_src = match fs::read_to_string(&ir_path) {
        Ok(content) => content,
        Err(err) => {
            eprintln!("Error reading file {}: {}", ir_path.display(), err);
            process::exit(1);
        }
    };

    let yul_src = utils::remove_comments(&yul_src);
    let block = match yul::ObjectParser::new().parse(&yul_src) {
        Ok(obj) => obj,
        Err(err) => {
            eprintln!("Error parsing yul file {}: {}", ir_path.display(), err);
            process::exit(1);
        }
    };

    let llvm_context = Context::create();
    let mut opts = Yul2IROptions {
        verbose,
        opt_level: args.opt_level.parse().unwrap(),
        ignore_unknown_linker_library: args.ignore_unknown_linker_library,
        disable_all_optimizers: args.disable_all_optimizers,
        minify_wasm_size: args.minify_wasm_size,
        no_binaryen_optimize: args.no_binaryen_optimize,
        enable_all_optimizers: args.enable_all_optimizers,
        enable_storage_load_store_little_endian: args.enable_little_endian_storage_load_store,
        ..Default::default()
    };

    for sym in &args.symbol {
        let parts: Vec<&str> = sym.split('=').collect();
        if parts.len() == 2 {
            let path = parts[0];
            let address = parts[1];
            opts.symbol2addr
                .insert(path.to_string(), address.to_string());
        }
    }

    opts.main_contract_name = block.name.clone();
    opts.output_dir = out_dir.to_string_lossy().to_string();

    let mut context = Yul2IRContext::new_with_object(&llvm_context, &opts, block);
    context.default_ret_type = match &args.default_ret_type {
        None => YulLowLevelValueType::U256,
        Some(string) => match string.as_str() {
            "u256" => YulLowLevelValueType::U256,
            "bytes32" => YulLowLevelValueType::Bytes32Pointer,
            str => {
                eprintln!(
                    "Invalid default return type: {}. Expected: u256, bytes32",
                    str
                );
                process::exit(1);
            }
        },
    };

    let wasm_basename = wasm_path.file_name().unwrap().to_str().unwrap();
    let emited_bc = match context.emit(wasm_basename) {
        Ok(bc) => bc,
        Err(err) => {
            eprintln!("Error emitting WebAssembly: {}", err);
            process::exit(1);
        }
    };

    fs::write(&wasm_path, &emited_bc).unwrap();

    // Generate cbin files
    let contract_cbin_path = wasm_path.with_extension("cbin");
    let wasmcode_length_big_endian_4bytes = (emited_bc.len() as u32).to_be_bytes();
    let contract_cbin_code = [wasmcode_length_big_endian_4bytes.to_vec(), emited_bc].concat();
    fs::write(&contract_cbin_path, &contract_cbin_code).unwrap();

    let contract_cbin_code_hex = hex::encode(&contract_cbin_code);
    let contract_cbin_hex_path = wasm_path.with_extension("cbin.hex");
    fs::write(&contract_cbin_hex_path, contract_cbin_code_hex).unwrap();

    println!(
        "Successfully compiled {} to {}",
        contract,
        wasm_path.display()
    );
}

/// Run tests for the compiled contracts
fn run_tests(contracts: &[String], out_dir: &Path, vm_address: &str, test_address: &str) {
    println!("Running tests...");

    // Find all test functions in the test contracts
    let test_vm_path = out_dir.join("WasmTestVM.wasm");

    for contract in contracts {
        if contract == "WasmTestVM" {
            continue;
        }

        let contract_path = out_dir.join(format!("{}.wasm", contract));
        let test_functions = find_test_functions(&contract_path);

        if test_functions.is_empty() {
            println!("No test functions found in contract: {}", contract);
            continue;
        }

        println!("Running tests for contract: {}", contract);

        // Initialize test environment
        init_test(&test_vm_path, vm_address, &contract_path, test_address);

        // Run each test function
        for func in test_functions {
            println!("  Running test: {}", func);
            let result = call_contract_function(&contract_path, test_address, &func);
            match result {
                Ok(_) => println!("    Test passed: {}", func),
                Err(e) => {
                    eprintln!("    Test failed: {}", func);
                    eprintln!("    Error: {}", e);
                    process::exit(1);
                }
            }
        }
    }
}

/// Find test functions in a contract
fn find_test_functions(contract_path: &PathBuf) -> Vec<String> {
    // This is a simplified implementation that looks for function names starting with "test"
    // A more robust implementation would analyze the WASM file to find all test functions

    let mut functions = Vec::new();

    // Try to use wasm-objdump if available
    let output = Command::new("wasm-objdump")
        .arg("-x")
        .arg(contract_path)
        .output();

    if let Ok(output) = output {
        if output.status.success() {
            let output_str = String::from_utf8_lossy(&output.stdout);
            for line in output_str.lines() {
                if line.contains(" func[") && line.contains("test") {
                    // Extract function name
                    if let Some(name_start) = line.rfind(' ') {
                        let func_name = line[name_start + 1..].trim();
                        if func_name.starts_with("test") {
                            functions.push(format!("{}()", func_name));
                        }
                    }
                }
            }
        }
    }

    // TODO: find tests in xx.t.sol xx.json, which have "IS_TEST"
    //       then find abis with "test" prefix and have no params

    // If wasm-objdump failed or found no functions, use a fallback approach
    if functions.is_empty() {
        // Common test function names in Forge
        let common_tests = [
            "testDeployAndTotalSupply()",
            "testMint()",
            "testApproveAndAllowance()",
            "testTransfer()",
            "testTransferFrom()",
            "testCompleteFlow()",
        ];

        functions.extend(common_tests.iter().map(|s| s.to_string()));
    }

    functions
}

/// Initialize the test environment
fn init_test(wasm_vm_file: &PathBuf, vm_addr: &str, contract_file: &PathBuf, contract_addr: &str) {
    println!("Initializing test environment...");
    println!("  WasmTestVM: {}", wasm_vm_file.display());
    println!("  VM Address: {}", vm_addr);
    println!("  Contract: {}", contract_file.display());
    println!("  Contract Address: {}", contract_addr);

    // Delete test.db if it exists
    let test_db_path = "test.db";
    if Path::new(test_db_path).exists() {
        fs::remove_file(test_db_path).unwrap_or_else(|e| {
            eprintln!("Error removing test.db: {}", e);
        });
    }

    // Deploy WasmTestVM
    let deploy_vm_status = Command::new("/opt/chain_mockcli")
        .arg("-f")
        .arg(wasm_vm_file)
        .arg("--action")
        .arg("deploy")
        .arg("-s")
        .arg("0x9988776655443322119900112233445566778899")
        .arg("-t")
        .arg(vm_addr)
        .arg("-i")
        .arg("0x")
        .status()
        .unwrap_or_else(|e| {
            eprintln!("Error deploying WasmTestVM: {}", e);
            process::exit(1);
        });

    if !deploy_vm_status.success() {
        eprintln!("Failed to deploy WasmTestVM");
        process::exit(1);
    }

    // Deploy test contract
    let deploy_contract_status = Command::new("/opt/chain_mockcli")
        .arg("-f")
        .arg(contract_file)
        .arg("--action")
        .arg("deploy")
        .arg("-s")
        .arg("0x9988776655443322119900112233445566778899")
        .arg("-t")
        .arg(contract_addr)
        .arg("-i")
        .arg("0x")
        .status()
        .unwrap_or_else(|e| {
            eprintln!("Error deploying test contract: {}", e);
            process::exit(1);
        });

    if !deploy_contract_status.success() {
        eprintln!("Failed to deploy test contract");
        process::exit(1);
    }

    // Call setUp() function
    let setup_result = call_contract_function(contract_file, contract_addr, "setUp()");
    if let Err(e) = setup_result {
        eprintln!("Failed to call setUp(): {}", e);
        process::exit(1);
    }
}

/// Call a contract function
fn call_contract_function(
    contract_file: &PathBuf,
    contract_addr: &str,
    function_name: &str,
) -> Result<(), String> {
    // Encode function selector
    let selector = encode_function_selector(function_name);

    let output = Command::new("/opt/chain_mockcli")
        .arg("-f")
        .arg(contract_file)
        .arg("-t")
        .arg(contract_addr)
        .arg("--action")
        .arg("call")
        .arg("--print-time")
        .arg("--enable-gas-meter")
        .arg("-s")
        .arg("0x9988776655443322119900112233445566778899")
        .arg("-i")
        .arg(selector)
        .output()
        .map_err(|e| e.to_string())?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        return Err(format!("Command failed: {}", stderr));
    }

    // Check if the output contains a success message
    if !stdout.contains("evm finish with result hex:") {
        return Err(format!("Test failed: {}", function_name));
    }

    Ok(())
}

/// Encode a function selector
fn encode_function_selector(function_name: &str) -> String {
    use keccak_hash::keccak;

    let selector = keccak(function_name.as_bytes());
    format!("0x{}", hex::encode(&selector.0[0..4]))
}
