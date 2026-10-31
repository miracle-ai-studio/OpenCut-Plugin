export {
  OpenCutClient,
  OpenCutApiError,
  OpenCutProtocolError,
  OpenCutTransportError,
  JobCompletionError,
  jobResult,
  requestEnvelope,
} from "../../../../packages/sdk-ts/src/index";
export type {
  AudioPreviewPcmHeader,
  ToolInput,
  ToolOutput,
  ToolName,
  Job,
  ArtifactReference,
  HostCapabilities,
  Envelope,
  ClientOptions,
  CallOptions,
} from "../../../../packages/sdk-ts/src/index";

import { OpenCutClient as Client } from "../../../../packages/sdk-ts/src/index";
import type { ClientOptions as Options } from "../../../../packages/sdk-ts/src/index";
import type { HostCapabilities } from "../../../../packages/sdk-ts/src/index";
type CapabilityConnection = {
  readonly sdk: Pick<Client, "hostCapabilities">;
  readonly connectionIdentity: string | null;
};
const capabilityReads = new WeakMap<CapabilityConnection, {
  identity: string;
  request: Promise<HostCapabilities>;
}>();

/** Coalesce concurrent editor discovery only within an authenticated connection.
 * Completed results are not cached; each consumer owns its returned value. */
export async function editorHostCapabilities(api: CapabilityConnection): Promise<HostCapabilities> {
  const identity = api.connectionIdentity;
  if (!identity) return api.sdk.hostCapabilities();
  let pending = capabilityReads.get(api);
  if (!pending || pending.identity !== identity) {
    pending = { identity, request: api.sdk.hostCapabilities() };
    capabilityReads.set(api, pending);
    const captured = pending;
    const clear = () => {
      if (capabilityReads.get(api) === captured) capabilityReads.delete(api);
    };
    void pending.request.then(clear, clear);
  }
  const capabilities = await pending.request;
  if (api.connectionIdentity !== identity) throw new Error("The connection changed during capability discovery.");
  return structuredClone(capabilities);
}

export type EditorConnection = Pick<Options, "baseUrl" | "token">;
/** Shared cookie/bearer transport assembly for the UI and its private workers. */
export function createEditorClient(connection: EditorConnection) {
  return new Client({
    ...connection,
    fetch: connection.token
      ? undefined
      : async (input, init) => {
          const headers = new Headers(init?.headers);
          headers.delete("Authorization");
          return fetch(input, { ...init, headers, credentials: "same-origin" });
        },
  });
}
