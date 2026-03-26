const { Connection, Keypair, PublicKey, SystemProgram, Transaction, sendAndConfirmTransaction } = require('@solana/web3.js');
const bs58Module = require('bs58');
const bs58 = bs58Module.default || bs58Module;

(async () => {
  const conn = new Connection('https://api.mainnet-beta.solana.com', 'confirmed');
  
  // Source: PumpPortal wallet
  const sourcePriv = process.env.PUMP_PORTAL_PRIVATE_KEY;
  if (!sourcePriv) {
    throw new Error('PUMP_PORTAL_PRIVATE_KEY not set');
  }
  
  const sourceKey = Keypair.fromSecretKey(bs58.decode(sourcePriv));
  const source = sourceKey.publicKey;
  
  // Destination: Bot wallet
  const destAddr = process.env.WALLET_PUBLIC_KEY;
  if (!destAddr) {
    throw new Error('WALLET_PUBLIC_KEY not set');
  }
  const dest = new PublicKey(destAddr);
  
  console.log('From:', source.toBase58());
  console.log('To:', dest.toBase58());
  
  const balance = await conn.getBalance(source);
  console.log('Source balance:', (balance / 1e9).toFixed(4), 'SOL');
  
  const amount = Math.floor(0.35 * 1e9);
  
  console.log('Transferring', (amount / 1e9).toFixed(2), 'SOL...');
  
  const tx = new Transaction().add(
    SystemProgram.transfer({
      fromPubkey: source,
      toPubkey: dest,
      lamports: amount
    })
  );
  
  const sig = await sendAndConfirmTransaction(conn, tx, [sourceKey], {
    commitment: 'confirmed',
    maxRetries: 3
  });
  
  console.log('✅ Transfer complete!');
  console.log('Signature:', sig);
  
  const newBalance = await conn.getBalance(dest);
  console.log('Bot wallet balance:', (newBalance / 1e9).toFixed(4), 'SOL');
})().catch(err => {
  console.error('❌ Error:', err.message);
  process.exit(1);
});
