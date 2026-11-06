export type NetworkSettings = {
  enabled: boolean;
  mode: 'auto' | 'fixed';
  allowedRegions: string[];
  nodeOrder: string[];
  fixedNodeId: string | null;
  excludedNodeIds: string[];
  regionOverrides: Record<string, string>;
};

export type NodeView = {
  id: string;
  name: string;
  protocol?: string;
  region: string;
  inferredRegion: string;
  health: 'unknown' | 'healthy' | 'degraded' | 'unhealthy';
  delayMs: number | null;
  lastCheckedAt: number | null;
  failureCount: number;
  cooldownUntil: number | null;
  excluded: boolean;
  businessSuccesses: number;
  businessFailures: number;
};

export type NetworkStatus = {
  settings: NetworkSettings;
  kernelRunning: boolean;
  currentNodeId: string | null;
  nodes: NodeView[];
  subscriptionName: string | null;
  subscriptionUpdatedAt: number | null;
  subscriptionKind: 'url' | 'file' | null;
  pendingUpdate: boolean;
  phase: string;
  error: string | null;
  lastSwitchReason: string | null;
  activeRequests?: number;
  skippedNodes?: number;
  importWarnings?: string[];
};
