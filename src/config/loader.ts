/**
 * @module config/loader
 * Config loader with JSON schema validation via ajv.
 * Supports loading from file, validating against schema, versioning,
 * and runtime patching with full audit trail.
 */

import fs from 'fs';
import path from 'path';
import Ajv from 'ajv';
import { PumpQuantConfig, ConfigVersion } from '../types/config';
import { createLogger } from '../utils/logger';
import { nowMs } from '../utils/time';

const log = createLogger('config');

// Resolve paths relative to project root
const PROJECT_ROOT = path.resolve(__dirname, '..', '..');
const SCHEMA_PATH = path.join(PROJECT_ROOT, 'config', 'schema.json');

/** Singleton config manager */
export class ConfigManager {
  private config: PumpQuantConfig;
  private version: number;
  private history: ConfigVersion[];
  private ajv: Ajv;
  private validate: ReturnType<Ajv['compile']>;

  constructor() {
    this.config = {} as PumpQuantConfig;
    this.version = 0;
    this.history = [];
    this.ajv = new Ajv({ allErrors: true, useDefaults: true });

    // Load and compile schema
    const schemaRaw = fs.readFileSync(SCHEMA_PATH, 'utf-8');
    const schema = JSON.parse(schemaRaw);
    this.validate = this.ajv.compile(schema);
  }

  /**
   * Load config from a JSON file path.
   * Validates against schema before accepting.
   */
  loadFromFile(configPath: string): PumpQuantConfig {
    const resolvedPath = path.isAbsolute(configPath)
      ? configPath
      : path.join(PROJECT_ROOT, configPath);

    if (!fs.existsSync(resolvedPath)) {
      throw new Error(`Config file not found: ${resolvedPath}`);
    }

    const raw = fs.readFileSync(resolvedPath, 'utf-8');
    let parsed: unknown;
    try {
      parsed = JSON.parse(raw);
    } catch (err) {
      throw new Error(`Invalid JSON in config file: ${resolvedPath}`);
    }

    return this.applyConfig(parsed as PumpQuantConfig, 'file', `Loaded from ${configPath}`);
  }

  /**
   * Apply a complete config object after validation.
   */
  applyConfig(
    newConfig: PumpQuantConfig,
    source: ConfigVersion['source'],
    description: string
  ): PumpQuantConfig {
    const valid = this.validate(newConfig);
    if (!valid) {
      const errors = this.validate.errors?.map(e => `${e.instancePath} ${e.message}`).join('; ');
      throw new Error(`Config validation failed: ${errors}`);
    }

    this.version += 1;
    this.config = structuredClone(newConfig);

    const snapshot: ConfigVersion = {
      version: this.version,
      config: structuredClone(this.config),
      timestamp: nowMs(),
      source,
      description,
    };
    this.history.push(snapshot);

    log.info(`Config loaded v${this.version}: ${description}`, { source, version: this.version });
    return this.config;
  }

  /**
   * Apply a partial patch to current config.
   * Deep merges patch into current config, re-validates, and versions.
   */
  applyPatch(
    patch: Partial<PumpQuantConfig>,
    source: ConfigVersion['source'],
    description: string
  ): PumpQuantConfig {
    const merged = this.deepMerge(
      structuredClone(this.config) as unknown as Record<string, unknown>,
      patch as unknown as Record<string, unknown>
    );
    return this.applyConfig(merged as unknown as PumpQuantConfig, source, description);
  }

  /** Get current config (immutable copy) */
  getConfig(): Readonly<PumpQuantConfig> {
    return this.config;
  }

  /** Get current config version number */
  getVersion(): number {
    return this.version;
  }

  /** Get config version history */
  getHistory(): readonly ConfigVersion[] {
    return this.history;
  }

  /** Get a specific version */
  getVersionSnapshot(version: number): ConfigVersion | undefined {
    return this.history.find(h => h.version === version);
  }

  /** Validate a config object without applying it */
  validateConfig(config: unknown): { valid: boolean; errors: string[] } {
    const valid = this.validate(config);
    const errors = valid ? [] : (this.validate.errors?.map(e => `${e.instancePath} ${e.message}`) || []);
    return { valid: !!valid, errors };
  }

  /** Deep merge utility */
  private deepMerge(target: Record<string, unknown>, source: Record<string, unknown>): Record<string, unknown> {
    for (const key of Object.keys(source)) {
      const sourceVal = source[key];
      const targetVal = target[key];

      if (
        sourceVal !== null &&
        typeof sourceVal === 'object' &&
        !Array.isArray(sourceVal) &&
        targetVal !== null &&
        typeof targetVal === 'object' &&
        !Array.isArray(targetVal)
      ) {
        target[key] = this.deepMerge(
          targetVal as Record<string, unknown>,
          sourceVal as Record<string, unknown>
        );
      } else {
        target[key] = sourceVal;
      }
    }
    return target;
  }
}

/** Global config manager singleton */
let _configManager: ConfigManager | null = null;

export function getConfigManager(): ConfigManager {
  if (!_configManager) {
    _configManager = new ConfigManager();
  }
  return _configManager;
}

/** Convenience: get current config */
export function getConfig(): Readonly<PumpQuantConfig> {
  return getConfigManager().getConfig();
}

/** Convenience: get current config version */
export function getConfigVersion(): number {
  return getConfigManager().getVersion();
}
