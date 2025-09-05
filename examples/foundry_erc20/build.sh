#!/bin/bash
set -e

source ../../tools/build_utils.sh

# install solidity
# https://docs.soliditylang.org/en/latest/installing-solidity.html

# install foundry
# curl -L https://foundry.paradigm.xyz | bash

setup_build_mode ${1:-release}

forge clean
forge build --extra-output-files ir-optimized
# for debug: forge build --extra-output-files ir
# ir generated in out/TokenFactory.sol/TokenFactory.ir

YUL_IR_PATH="out"
# contracts to compile
CONTRACTS=(
    "MyToken"
    "TokenFactory"
)

compile_all_contracts CONTRACTS[@] "$YUL_IR_PATH"
