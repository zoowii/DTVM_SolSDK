// Copyright (C) 2025 the DTVM authors. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! Forge test integration for yul2wasm
//!
//! This module provides functionality to run Foundry tests with yul2wasm compilation.
//! It automatically discovers test contracts, compiles them to WASM, and executes them
//! using the chain_mockcli test environment.
//!
//! Example usage:
//! ```bash
//! yul2wasm forge-test --project-dir ./examples/foundry_erc20 --verbose
//! ```

use crate::yul2ir::config::Yul2IROptions;
use crate::yul2ir::context::Yul2IRContext;
use crate::yul2ir::utils;
use crate::yul2ir::yul;
use crate::yul2ir::yul_instruction::YulLowLevelValueType;
use crate::Args;
use clap::arg;
use clap::Parser;
use inkwell::context::Context;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{self, Command};

#[derive(Debug, Serialize, Deserialize)]
struct TestResult {
    contract: String,
    function: String,
    status: String,
    error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct TestSummary {
    total: usize,
    passed: usize,
    failed: usize,
    results: Vec<TestResult>,
}

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
    #[arg(long = "out-dir", help = "Output directory for compiled tests")]
    out_dir: Option<String>,

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

    /// Path to chain_mockcli executable
    #[arg(
        long = "mockcli-path",
        help = "Path to chain_mockcli executable",
        default_value = "/opt/chain_mockcli"
    )]
    mockcli_path: String,

    /// Path to WasmTestVM.sol file
    #[arg(long = "test-vm-path", help = "Path to WasmTestVM.sol file")]
    test_vm_path: Option<String>,

    /// Filter tests by name pattern
    #[arg(long = "filter", help = "Run only tests matching this pattern")]
    filter: Option<String>,

    /// Verbose output
    #[arg(long = "verbose", help = "Verbose output")]
    verbose: bool,

    /// Debug mode
    #[arg(long = "debug", help = "Debug mode")]
    debug: bool,

    /// Build mode (debug or release)
    #[arg(long = "build-mode", help = "Build mode", default_value = "release")]
    build_mode: String,

    /// Skip forge test execution, only compile
    #[arg(
        long = "compile-only",
        help = "Only compile contracts, don't run tests"
    )]
    compile_only: bool,

    /// Output format for test results
    #[arg(
        long = "output-format",
        help = "Output format for test results",
        default_value = "human",
        value_parser = ["human", "json"]
    )]
    output_format: String,
}

/// Run forge tests for the project
pub fn run_forge_tests(forge_args: &ForgeTestArgs, args: &Args) {
    if forge_args.verbose {
        println!("Running forge tests in {} mode...", forge_args.build_mode);
        println!("Project directory: {}", forge_args.project_dir);
    }

    // Setup project directory
    let project_dir = PathBuf::from(&forge_args.project_dir);
    if !project_dir.exists() {
        eprintln!(
            "Error: Project directory does not exist: {}",
            project_dir.display()
        );
        process::exit(1);
    }

    // Determine output directory
    let out_dir = if let Some(out_dir_str) = &forge_args.out_dir {
        PathBuf::from(out_dir_str)
    } else {
        // Auto-detect output directory
        detect_output_directory(&project_dir)
    };

    if forge_args.verbose {
        println!("Output directory: {}", out_dir.display());
    }

    // Create output directory if it doesn't exist
    if !out_dir.exists() {
        fs::create_dir_all(&out_dir).unwrap_or_else(|e| {
            eprintln!("Error creating output directory: {}", e);
            process::exit(1);
        });
    }

    // Step 1: Find WasmTestVM.sol and copy it to the src directory
    let wasmtestvm_path = find_wasmtestvm(&project_dir, forge_args.test_vm_path.as_deref());
    let src_dir = project_dir.join("src");
    if !src_dir.exists() {
        fs::create_dir_all(&src_dir).unwrap_or_else(|e| {
            eprintln!("Error creating src directory: {}", e);
            process::exit(1);
        });
    }

    let target_vm_path = src_dir.join("WasmTestVM.sol");
    if forge_args.verbose {
        println!(
            "Copying WasmTestVM.sol from {} to {}",
            wasmtestvm_path.display(),
            target_vm_path.display()
        );
    }
    fs::copy(wasmtestvm_path, &target_vm_path).unwrap_or_else(|e| {
        eprintln!("Error copying WasmTestVM.sol: {}", e);
        process::exit(1);
    });

    // Step 2: Run forge test to generate IR files
    if forge_args.verbose {
        println!("Running forge test to generate IR files...");
    }
    let mut forge_cmd = Command::new("forge");
    forge_cmd
        .current_dir(&project_dir)
        .arg("test")
        .arg("--extra-output-files")
        .arg("ir-optimized");

    if !forge_args.verbose {
        forge_cmd.arg("--silent");
    }

    let forge_status = forge_cmd.status().unwrap_or_else(|e| {
        eprintln!("Error running forge test: {}", e);
        eprintln!("Make sure Foundry is installed and 'forge' is in your PATH");
        process::exit(1);
    });

    if !forge_status.success() {
        eprintln!("Forge test failed. Check your Solidity contracts for compilation errors.");
        process::exit(1);
    }

    // Step 3: Find all contracts in the out directory
    let mut contracts = vec!["WasmTestVM".to_string()];
    let test_contracts = find_test_contracts(&out_dir, forge_args.filter.as_deref());
    contracts.extend(test_contracts);

    if forge_args.verbose {
        println!("Found contracts to compile: {:?}", contracts);
    }

    if contracts.len() == 1 {
        println!("No test contracts found. Make sure your test files follow Foundry naming conventions (*.t.sol)");
        process::exit(1);
    }

    // Step 4: Compile all contracts
    for contract in &contracts {
        compile_contract(contract, &out_dir, args, forge_args.verbose);
    }

    if forge_args.compile_only {
        println!("Compilation completed. Skipping test execution.");
        fs::remove_file(target_vm_path).ok();
        return;
    }

    // Step 5: Run the tests
    run_tests(
        &contracts,
        &out_dir,
        &forge_args.vm_address,
        &forge_args.test_address,
        &forge_args.mockcli_path,
        forge_args.filter.as_deref(),
        &forge_args.output_format,
        forge_args.verbose,
    );

    // Cleanup
    fs::remove_file(target_vm_path).ok();

    if forge_args.verbose {
        println!("All forge tests completed successfully!");
    }
}

/// Detect the output directory for a Foundry project
fn detect_output_directory(project_dir: &Path) -> PathBuf {
    // Check for foundry.toml configuration
    let foundry_toml = project_dir.join("foundry.toml");
    if foundry_toml.exists() {
        if let Ok(content) = fs::read_to_string(&foundry_toml) {
            // Simple parsing for out directory
            for line in content.lines() {
                if line.trim().starts_with("out") && line.contains('=') {
                    if let Some(out_dir) = line.split('=').nth(1) {
                        let out_dir = out_dir.trim().trim_matches('"').trim_matches('\'');
                        return project_dir.join(out_dir);
                    }
                }
            }
        }
    }

    // Default to "out" directory
    project_dir.join("out")
}

// TODO: put WasmTestVM logic in chain_mockcli
/// Find WasmTestVM.sol in common locations
fn find_wasmtestvm(project_dir: &Path, custom_path: Option<&str>) -> PathBuf {
    // If custom path is provided, use it
    if let Some(custom_path) = custom_path {
        let p = PathBuf::from(custom_path);
        if p.exists() {
            return p;
        } else {
            eprintln!(
                "Error: Custom WasmTestVM.sol path does not exist: {}",
                custom_path
            );
            process::exit(1);
        }
    }

    let possible_paths = [
        project_dir.join("scripts/WasmTestVM.sol"),
        project_dir.join("../scripts/WasmTestVM.sol"),
        PathBuf::from("scripts/WasmTestVM.sol"),
        PathBuf::from("../scripts/WasmTestVM.sol"),
        PathBuf::from("examples/scripts/WasmTestVM.sol"),
        PathBuf::from("../examples/scripts/WasmTestVM.sol"),
        PathBuf::from("../../examples/scripts/WasmTestVM.sol"),
    ];

    for path in &possible_paths {
        if path.exists() {
            return path.clone();
        }
    }

    eprintln!("Error: Could not find WasmTestVM.sol");
    eprintln!("Searched in the following locations:");
    for path in &possible_paths {
        eprintln!("  - {}", path.display());
    }
    eprintln!("Use --test-vm-path to specify a custom location");
    process::exit(1);
}

/// Find test contracts in the out directory
fn find_test_contracts(out_dir: &PathBuf, filter: Option<&str>) -> Vec<String> {
    let mut contracts = Vec::new();

    if let Ok(entries) = fs::read_dir(out_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                let file_name = path.file_name().unwrap().to_string_lossy().to_string();
                if file_name.ends_with(".sol") {
                    let contract_name = file_name.trim_end_matches(".sol").to_string();
                    if contract_name != "WasmTestVM" && is_test_contract(&contract_name, &path) {
                        // Apply filter if provided
                        if let Some(filter_pattern) = filter {
                            if !contract_name.contains(filter_pattern) {
                                continue;
                            }
                        }
                        contracts.push(contract_name);
                    }
                }
            }
        }
    }

    contracts
}

/// Check if a contract is a test contract by examining its metadata
fn is_test_contract(contract_name: &str, contract_dir: &Path) -> bool {
    // Check naming convention first
    if contract_name.starts_with("Test")
        || contract_name.ends_with("Test")
        || contract_name.contains("Test")
    {
        // Look for contract metadata to confirm it's a test
        let metadata_path = contract_dir.join(format!("{}.json", contract_name));
        if metadata_path.exists() {
            if let Ok(content) = fs::read_to_string(&metadata_path) {
                if let Ok(json) = serde_json::from_str::<Value>(&content) {
                    // Check if contract has IS_TEST constant or inherits from Test
                    if let Some(abi) = json.get("abi").and_then(|v| v.as_array()) {
                        for item in abi {
                            if let Some(name) = item.get("name").and_then(|v| v.as_str()) {
                                if name == "IS_TEST" {
                                    return true;
                                }
                            }
                        }
                    }
                }
            }
        }
        // If metadata check fails, rely on naming convention
        return true;
    }
    false
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
fn run_tests(
    contracts: &[String],
    out_dir: &Path,
    vm_address: &str,
    test_address: &str,
    mockcli_path: &str,
    filter: Option<&str>,
    output_format: &str,
    verbose: bool,
) {
    if verbose {
        println!("Running tests...");
    }

    // Find all test functions in the test contracts
    let test_vm_path = out_dir.join("WasmTestVM.wasm");
    let mut test_results = Vec::new();
    let mut total_tests = 0;
    let mut passed_tests = 0;

    for contract in contracts {
        if contract == "WasmTestVM" {
            continue;
        }

        let contract_path = out_dir.join(format!("{}.wasm", contract));
        let contract_dir = out_dir.join(format!("{}.sol", contract));
        let test_functions = find_test_functions(&contract_path, &contract_dir, filter);

        if test_functions.is_empty() {
            if verbose {
                println!("No test functions found in contract: {}", contract);
            }
            continue;
        }

        if output_format == "human" {
            println!("Running tests for contract: {}", contract);
        }

        // Initialize test environment
        init_test(
            &test_vm_path,
            vm_address,
            &contract_path,
            test_address,
            mockcli_path,
            verbose,
        );

        // Run each test function
        for func in test_functions {
            total_tests += 1;
            if output_format == "human" {
                print!("  Running test: {}...", func);
                std::io::Write::flush(&mut std::io::stdout()).unwrap();
            }

            let result = call_contract_function(&contract_path, test_address, &func, mockcli_path);
            match result {
                Ok(_) => {
                    passed_tests += 1;
                    if output_format == "human" {
                        println!(" PASS");
                    }
                    test_results.push(TestResult {
                        contract: contract.clone(),
                        function: func,
                        status: "PASS".to_string(),
                        error: None,
                    });
                }
                Err(e) => {
                    if output_format == "human" {
                        println!(" FAIL");
                        eprintln!("    Error: {}", e);
                    }
                    test_results.push(TestResult {
                        contract: contract.clone(),
                        function: func.clone(),
                        status: "FAIL".to_string(),
                        error: Some(e.clone()),
                    });

                    if output_format == "human" {
                        eprintln!("Test failed: {}", func);
                        process::exit(1);
                    }
                }
            }
        }
    }

    // Output results
    match output_format {
        "json" => {
            let summary = TestSummary {
                total: total_tests,
                passed: passed_tests,
                failed: total_tests - passed_tests,
                results: test_results,
            };
            println!("{}", serde_json::to_string_pretty(&summary).unwrap());
        }
        _ => {
            println!("\nTest Summary:");
            println!("  Total: {}", total_tests);
            println!("  Passed: {}", passed_tests);
            println!("  Failed: {}", total_tests - passed_tests);

            if total_tests - passed_tests > 0 {
                process::exit(1);
            }
        }
    }
}

/// Find test functions in a contract
fn find_test_functions(
    contract_path: &PathBuf,
    contract_dir: &Path,
    filter: Option<&str>,
) -> Vec<String> {
    let mut functions = Vec::new();

    // First, try to find test functions from contract metadata
    let metadata_path = contract_dir.join(format!(
        "{}.json",
        contract_path.file_stem().unwrap().to_string_lossy()
    ));

    if metadata_path.exists() {
        if let Ok(content) = fs::read_to_string(&metadata_path) {
            if let Ok(json) = serde_json::from_str::<Value>(&content) {
                if let Some(abi) = json.get("abi").and_then(|v| v.as_array()) {
                    for item in abi {
                        if let Some(func_type) = item.get("type").and_then(|v| v.as_str()) {
                            if func_type == "function" {
                                if let Some(name) = item.get("name").and_then(|v| v.as_str()) {
                                    if name.starts_with("test") {
                                        // Check if function has no inputs (typical for test functions)
                                        let has_inputs = item
                                            .get("inputs")
                                            .and_then(|v| v.as_array())
                                            .map(|arr| !arr.is_empty())
                                            .unwrap_or(false);

                                        if !has_inputs {
                                            let func_sig = format!("{}()", name);
                                            // Apply filter if provided
                                            if let Some(filter_pattern) = filter {
                                                if name.contains(filter_pattern) {
                                                    functions.push(func_sig);
                                                }
                                            } else {
                                                functions.push(func_sig);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // If no functions found from metadata, try wasm-objdump as fallback
    if functions.is_empty() {
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
                                let func_sig = format!("{}()", func_name);
                                // Apply filter if provided
                                if let Some(filter_pattern) = filter {
                                    if func_name.contains(filter_pattern) {
                                        functions.push(func_sig);
                                    }
                                } else {
                                    functions.push(func_sig);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // If still no functions found, use fallback approach with common test names
    if functions.is_empty() {
        let common_tests = [
            "testDeployAndTotalSupply()",
            "testMint()",
            "testApproveAndAllowance()",
            "testTransfer()",
            "testTransferFrom()",
            "testCompleteFlow()",
        ];

        for test in &common_tests {
            // Apply filter if provided
            if let Some(filter_pattern) = filter {
                if test.contains(filter_pattern) {
                    functions.push(test.to_string());
                }
            } else {
                functions.push(test.to_string());
            }
        }
    }

    functions
}

/// Initialize the test environment
fn init_test(
    wasm_vm_file: &PathBuf,
    vm_addr: &str,
    contract_file: &PathBuf,
    contract_addr: &str,
    mockcli_path: &str,
    verbose: bool,
) {
    if verbose {
        println!("Initializing test environment...");
        println!("  WasmTestVM: {}", wasm_vm_file.display());
        println!("  VM Address: {}", vm_addr);
        println!("  Contract: {}", contract_file.display());
        println!("  Contract Address: {}", contract_addr);
    }

    // Delete test.db if it exists
    let test_db_path = "test.db";
    if Path::new(test_db_path).exists() {
        fs::remove_file(test_db_path).unwrap_or_else(|e| {
            eprintln!("Error removing test.db: {}", e);
        });
    }

    // Deploy WasmTestVM
    let deploy_vm_status = Command::new(mockcli_path)
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
            eprintln!("Make sure chain_mockcli is available at: {}", mockcli_path);
            process::exit(1);
        });

    if !deploy_vm_status.success() {
        eprintln!("Failed to deploy WasmTestVM");
        process::exit(1);
    }

    // Deploy test contract
    let deploy_contract_status = Command::new(mockcli_path)
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
    let setup_result =
        call_contract_function(contract_file, contract_addr, "setUp()", mockcli_path);
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
    mockcli_path: &str,
) -> Result<(), String> {
    // Encode function selector
    let selector = encode_function_selector(function_name);

    let output = Command::new(mockcli_path)
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
        .map_err(|e| format!("Failed to execute {}: {}", mockcli_path, e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        return Err(format!("Command failed: {}", stderr));
    }

    // Check if the output contains a success message
    if !stdout.contains("evm finish with result hex:") {
        // Check for revert messages
        if stdout.contains("evm revert") {
            return Err(format!("Test reverted: {}", function_name));
        }
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
