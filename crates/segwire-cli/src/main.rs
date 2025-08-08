use anyhow::Result;

mod dbus_client;

use dbus_client::DbusClient;

#[monoio::main]
async fn main() -> Result<()> {
    // Initialize basic logging
    tracing_subscriber::fmt::init();
    
    println!("Segwire CLI");
    
    // Test D-Bus client connection and discovery
    match test_dbus_connection().await {
        Ok(_) => println!("D-Bus connection test successful"),
        Err(e) => {
            eprintln!("D-Bus connection test failed: {}", e);
            eprintln!("Note: This is expected if segwire-daemon is not running");
        }
    }
    
    Ok(())
}

/// Test the D-Bus client connection and service discovery
async fn test_dbus_connection() -> Result<()> {
    println!("Testing D-Bus client connection and service discovery...");
    
    // Attempt to create a D-Bus client
    let client = DbusClient::new().await?;
    
    // Display connection information
    let conn_info = client.get_connection_info();
    println!("Connection established:");
    println!("{}", conn_info);
    
    // Test service availability
    if client.is_service_available().await {
        println!("✓ Daemon service is available and responding");
        
        // Try to discover available methods
        match client.discover_methods().await {
            Ok(methods) => {
                println!("✓ Available methods discovered: {} methods", methods.len());
                for method in methods.iter().take(5) { // Show first 5 methods
                    println!("  - {}", method);
                }
                if methods.len() > 5 {
                    println!("  ... and {} more", methods.len() - 5);
                }
            }
            Err(e) => {
                println!("⚠ Method discovery failed: {}", e);
            }
        }
        
        // Test connection with a simple method call
        match client.test_connection().await {
            Ok(_) => println!("✓ Connection test successful"),
            Err(e) => println!("⚠ Connection test failed: {}", e),
        }
    } else {
        println!("⚠ Daemon service is not available");
    }
    
    Ok(())
}