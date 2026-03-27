#!/usr/bin/env node
/**
 * Wallet manager CLI
 * Usage:
 *   node scripts/wallet-manager.js list
 *   node scripts/wallet-manager.js add <base58_private_key> <label>
 *   node scripts/wallet-manager.js generate <label>
 *   node scripts/wallet-manager.js export <label>   (shows public key only — never logs private key)
 */
const { Keypair } = require('@solana/web3.js');
const bs58 = require('bs58');
const path = require('path');
// Load EncryptedWalletStore from dist
const { EncryptedWalletStore } = require('../dist/mev/wallet-store');

const store = new EncryptedWalletStore();
const [,, cmd, arg1, arg2] = process.argv;

switch (cmd) {
  case 'list': {
    const wallets = store.listWallets();
    if (!wallets.length) { console.log('No wallets stored.'); break; }
    wallets.forEach(w => console.log(`${w.label}: ${w.publicKey} (added ${w.addedAt})`));
    break;
  }
  case 'add': {
    if (!arg1 || !arg2) { console.log('Usage: add <base58_key> <label>'); break; }
    const secret = bs58.default ? bs58.default.decode(arg1) : bs58.decode(arg1);
    const kp = Keypair.fromSecretKey(secret);
    store.addWallet(kp, arg2);
    console.log(`Added: ${kp.publicKey.toBase58()}`);
    break;
  }
  case 'generate': {
    const label = arg1 || `wallet-${Date.now()}`;
    const kp = Keypair.generate();
    store.addWallet(kp, label);
    console.log(`Generated: ${kp.publicKey.toBase58()} (label: ${label})`);
    console.log('⚠️  Fund this wallet before going live. Private key is encrypted in data/wallets.enc');
    break;
  }
  case 'export': {
    console.log('Public keys only (private keys never printed):');
    store.listWallets().forEach(w => console.log(`${w.label}: ${w.publicKey}`));
    break;
  }
  default:
    console.log('Commands: list | add <key> <label> | generate <label> | export <label>');
}
