import { createContext } from "react";
import type { ScanDetectorConfig } from "./scanDetector";

/**
 * The shop's scanner settings (`scanner.suffix`, `scanner.max_key_gap_ms`, E04-T03), applied
 * to every scan capture under it. Empty means the built-in defaults.
 */
export const ScanPreferences = createContext<Partial<ScanDetectorConfig>>({});
