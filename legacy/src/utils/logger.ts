/**
 * @module utils/logger
 * Winston-based logger with structured output
 */

import winston from 'winston';

const LOG_LEVEL = process.env.LOG_LEVEL || 'info';

const logger = winston.createLogger({
  level: LOG_LEVEL,
  format: winston.format.combine(
    winston.format.timestamp({ format: 'YYYY-MM-DD HH:mm:ss.SSS' }),
    winston.format.errors({ stack: true }),
    winston.format.json()
  ),
  defaultMeta: { service: 'pump-quant' },
  transports: [
    new winston.transports.Console({
      format: winston.format.combine(
        winston.format.colorize(),
        winston.format.printf(({ timestamp, level, message, service, ...meta }) => {
          const metaStr = Object.keys(meta).length > 0
            ? ` ${JSON.stringify(meta)}`
            : '';
          return `${timestamp} [${service}] ${level}: ${message}${metaStr}`;
        })
      ),
    }),
  ],
});

/** Create a child logger with a specific component label */
export function createLogger(component: string): winston.Logger {
  return logger.child({ component });
}

export default logger;
