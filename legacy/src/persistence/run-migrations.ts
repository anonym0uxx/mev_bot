/**
 * @module persistence/run-migrations
 * CLI entrypoint to run database migrations
 */

import dotenv from 'dotenv';
dotenv.config();

import { getDatabase } from './database';

const db = getDatabase();
console.log('Migrations complete.');
db.close();
