use std::io::{self, Read};
use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;
use clarity::vm::types::QualifiedContractIdentifier;
use stacks_common::address::b58;
use stacks_common::types::chainstate::StacksPrivateKey;

use crate::config::Network;

extern crate alloc;

#[derive(Parser, Debug)]
#[command(author, version, about)]
/// The CLI arguments for the stacks signer
pub struct Cli {
    /// Subcommand action to take
    #[command(subcommand)]
    pub command: Command,
}

/// Subcommands for the stacks signer binary
#[derive(clap::Subcommand, Debug)]
pub enum Command {
    /// Get a chunk from the stacker-db instance
    GetChunk(GetChunkArgs),
    /// Get the latest chunk from the stacker-db instance
    GetLatestChunk(GetLatestChunkArgs),
    /// List chunks from the stacker-db instance
    ListChunks(StackerDBArgs),
    /// Upload a chunk to the stacker-db instance
    PutChunk(PutChunkArgs),
    /// Run DKG and sign the message through the stacker-db instance
    DkgSign(SignArgs),
    /// Sign the message through the stacker-db instance
    Sign(SignArgs),
    /// Run a DKG round through the stacker-db instance
    Dkg(RunDkgArgs),
    /// Run the signer, waiting for events from the stacker-db instance
    Run(RunDkgArgs),
    /// Generate necessary files for running a collection of signers
    GenerateFiles(GenerateFilesArgs),
}

/// Basic arguments for all cyrptographic and stacker-db functionality
#[derive(Parser, Debug, Clone)]
pub struct StackerDBArgs {
    /// The Stacks node to connect to
    #[arg(long)]
    pub host: SocketAddr,
    /// The stacker-db contract to use. Must be in the format of "STACKS_ADDRESS.CONTRACT_NAME"
    #[arg(short, long, value_parser = parse_contract)]
    pub contract: QualifiedContractIdentifier,
}

/// Arguments for the get-chunk command
#[derive(Parser, Debug, Clone)]
pub struct GetChunkArgs {
    /// The base arguments
    #[clap(flatten)]
    pub db_args: StackerDBArgs,
    /// The slot ID to get
    #[arg(long)]
    pub slot_id: u32,
    /// The slot version to get
    #[arg(long)]
    pub slot_version: u32,
}

/// Arguments for the get-latest-chunk command
#[derive(Parser, Debug, Clone)]
pub struct GetLatestChunkArgs {
    /// The base arguments
    #[clap(flatten)]
    pub db_args: StackerDBArgs,
    /// The slot ID to get
    #[arg(long)]
    pub slot_id: u32,
}

#[derive(Parser, Debug, Clone)]
/// Arguments for the put-chunk command
pub struct PutChunkArgs {
    /// The base arguments
    #[clap(flatten)]
    pub db_args: StackerDBArgs,
    /// The Stacks private key to use in hexademical format
    #[arg(short, long, value_parser = parse_private_key)]
    pub private_key: StacksPrivateKey,
    /// The slot ID to get
    #[arg(long)]
    pub slot_id: u32,
    /// The slot version to get
    #[arg(long)]
    pub slot_version: u32,
    /// The data to upload
    #[arg(required = false, value_parser = parse_data)]
    // Note this weirdness is due to https://github.com/clap-rs/clap/discussions/4695
    // Need to specify the long name here due to invalid parsing in Clap which looks at the NAME rather than the TYPE which causes issues in how it handles Vec's.
    pub data: alloc::vec::Vec<u8>,
}

#[derive(Parser, Debug, Clone)]
/// Arguments for the dkg-sign and sign command
pub struct SignArgs {
    /// Path to config file
    #[arg(long, value_name = "FILE")]
    pub config: PathBuf,
    /// The data to sign
    #[arg(required = false, value_parser = parse_data)]
    // Note this weirdness is due to https://github.com/clap-rs/clap/discussions/4695
    // Need to specify the long name here due to invalid parsing in Clap which looks at the NAME rather than the TYPE which causes issues in how it handles Vec's.
    pub data: alloc::vec::Vec<u8>,
}

#[derive(Parser, Debug, Clone)]
/// Arguments for the Run and Dkg commands
pub struct RunDkgArgs {
    /// Path to config file
    #[arg(long, value_name = "FILE")]
    pub config: PathBuf,
}

#[derive(Parser, Debug, Clone)]
/// Arguments for the generate-files command
pub struct GenerateFilesArgs {
    /// The base arguments
    #[clap(flatten)]
    pub db_args: StackerDBArgs,
    #[arg(
        long,
        required_unless_present = "private_keys",
        conflicts_with = "private_keys"
    )]
    /// The number of signers to generate
    pub num_signers: Option<u32>,
    #[clap(long, value_name = "FILE")]
    /// A path to a file containing a list of hexadecimal Stacks private keys
    pub private_keys: Option<PathBuf>,
    #[arg(long)]
    /// The total number of key ids to distribute among the signers
    pub num_keys: u32,
    #[arg(long, value_parser = parse_network)]
    /// The network to use. One of "mainnet", "testnet", or "mocknet".
    pub network: Network,
    /// The directory to write the test data files to
    #[arg(long, default_value = ".")]
    pub dir: PathBuf,
    /// The number of milliseconds to wait when polling for events from the stacker-db instance.
    #[arg(long)]
    pub timeout: Option<u64>,
}

/// Parse the contract ID
fn parse_contract(contract: &str) -> Result<QualifiedContractIdentifier, String> {
    QualifiedContractIdentifier::parse(contract).map_err(|e| format!("Invalid contract: {}", e))
}

/// Parse the hexadecimal Stacks private key
fn parse_private_key(private_key: &str) -> Result<StacksPrivateKey, String> {
    StacksPrivateKey::from_hex(private_key).map_err(|e| format!("Invalid private key: {}", e))
}

/// Parse the input data
fn parse_data(data: &str) -> Result<Vec<u8>, String> {
    let encoded_data = if data == "-" {
        // Parse the data from stdin
        let mut data = String::new();
        io::stdin().read_to_string(&mut data).unwrap();
        data
    } else {
        data.to_string()
    };
    let data =
        b58::from(&encoded_data).map_err(|e| format!("Failed to decode provided data: {}", e))?;
    Ok(data)
}

/// Parse the network. Must be one of "mainnet", "testnet", or "mocknet".
fn parse_network(network: &str) -> Result<Network, String> {
    Ok(match network.to_lowercase().as_str() {
        "mainnet" => Network::Mainnet,
        "testnet" => Network::Testnet,
        "mocknet" => Network::Mocknet,
        _ => {
            return Err(format!(
                "Invalid network: {}. Must be one of \"mainnet\", \"testnet\", or \"mocknet\".",
                network
            ))
        }
    })
}
