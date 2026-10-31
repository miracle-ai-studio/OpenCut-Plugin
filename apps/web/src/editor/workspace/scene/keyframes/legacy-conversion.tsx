import { editorHostCapabilities } from "../../../../lib/editor-sdk";
import { useEffect, useRef, useState } from "react";
import type { ToolInput, ToolOutput } from "../../../../lib/editor-sdk";
import { useWorkspace } from "../../context";
import { useLocale } from "../../../shared/locale";
import type { KeyframeEdit, ScalarChannel } from "./types";

type Request = ToolInput<"opencut_keyframe_legacy_conversion_plan">;
type Result = ToolOutput<"opencut_keyframe_legacy_conversion_plan">;
type Owner = Extract<KeyframeEdit, { kind: "channel_put" }>["owner"];
const profile = "opencut-legacy-decimal12-lookup32-linear-v1";
const message = (error: unknown) =>
  error instanceof Error ? error.message : String(error);

/** A separate explicit review, never a hidden prerequisite performed by trim. */
export function LegacyKeyframeConversion({
  channel,
  owner,
  disabled,
  isBlocked,
  onChooseHold,
}: {
  channel: ScalarChannel;
  owner: Owner;
  disabled: boolean;
  isBlocked(): boolean;
  onChooseHold(channel: ScalarChannel, owner: Owner): void;
}) {
  const { api, scene, controller } = useWorkspace();
  const locale = useLocale();
  const zh = locale.language === "zh";
  const text = (en: string, cn: string) => (zh ? cn : en);
  const [available, setAvailable] = useState(false);
  const [tolerance, setTolerance] = useState("0.000001");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [review, setReview] = useState<{
    result: Result;
    fence(): void;
  } | null>(null);
  const live = useRef(true),
    epoch = useRef(0),
    flight = useRef(false);
  const selectedId = scene?.selectedId;
  const scope = `${scene?.head.project.id}\0${scene?.head.revision}\0${scene?.sequenceId}\0${selectedId}\0${channel.id}`;
  const legacy = channel.keys.filter(
    (key, i) =>
      i + 1 < channel.keys.length &&
      key.segment_to_next.kind === "legacy_normalized_bezier",
  );
  const legacyIds = JSON.stringify(legacy.map((key) => key.id));
  const [chosen, setChosen] = useState(
    () => new Set(legacy.map((key) => key.id)),
  );
  const chosenIds = JSON.stringify(
    legacy.filter((key) => chosen.has(key.id)).map((key) => key.id),
  );
  const inputIdentity = `${scope}\0${tolerance}\0${legacyIds}\0${chosenIds}`;
  const currentInput = useRef(inputIdentity);
  currentInput.current = inputIdentity;
  const boundary =
    (channel.extrapolation.before === "linear" &&
      chosen.has(channel.keys[0]?.id ?? "")) ||
    (channel.extrapolation.after === "linear" &&
      chosen.has(channel.keys.at(-2)?.id ?? ""));
  useEffect(() => {
    setChosen(new Set(legacy.map((key) => key.id)));
  }, [scope, legacyIds]);
  useEffect(() => {
    let active = true;
    setAvailable(false);
    if (api)
      void editorHostCapabilities(api)
        .then((c) => {
          if (active)
            setAvailable(
              c.tools.includes("opencut_keyframe_legacy_conversion_plan"),
            );
        })
        .catch((e) => {
          if (active) setError(message(e));
        });
    return () => {
      active = false;
    };
  }, [api, api?.connectionIdentity]);
  useEffect(() => {
    epoch.current++;
    setReview(null);
    setError(null);
  }, [api, api?.connectionIdentity, scope, tolerance, legacyIds, chosenIds]);
  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
      epoch.current++;
    };
  }, []);
  async function prepare() {
    if (
      !api ||
      !scene ||
      disabled ||
      flight.current ||
      boundary ||
      chosenIds === "[]"
    )
      return;
    const value = Number(tolerance);
    if (!tolerance.trim() || !Number.isFinite(value) || value < 0) {
      setError(
        text(
          "Enter a finite nonnegative absolute tolerance.",
          "请输入有限且非负的绝对误差容限。",
        ),
      );
      return;
    }
    const capturedApi = api,
      identity = api.connectionIdentity,
      serial = epoch.current,
      capturedInput = inputIdentity;
    const project = scene.head.project.id,
      revision = scene.head.revision,
      sequence = scene.sequenceId;
    const fence = () => {
      const state = controller.getSnapshot(),
        current = state.scene;
      if (
        !live.current ||
        epoch.current !== serial ||
        currentInput.current !== capturedInput ||
        isBlocked() ||
        state.api !== capturedApi ||
        state.api?.connectionIdentity !== identity ||
        current?.head.project.id !== project ||
        current.head.revision !== revision ||
        current.sequenceId !== sequence ||
        current.selectedId !== selectedId ||
        current.historical ||
        state.busy ||
        current.recovery?.record ||
        current.recovery?.error
      )
        throw new Error(
          text(
            "The conversion review changed. Prepare it again from the current revision.",
            "转换审查的目标已变化，请基于当前修订重新生成方案。",
          ),
        );
    };
    const request: Request = {
      project_id: project,
      expected_revision: revision,
      idempotency_key: crypto.randomUUID(),
      label: "Convert reviewed legacy lookup",
      sequence_id: sequence,
      channel_id: channel.id,
      left_key_ids: legacy.filter((key) => chosen.has(key.id)).map((k) => k.id),
      maximum_absolute_error: value,
    };
    flight.current = true;
    setPending(true);
    setError(null);
    setReview(null);
    try {
      fence();
      const result = await capturedApi.sdk.call(
        "opencut_keyframe_legacy_conversion_plan",
        request,
      );
      fence();
      if (
        result.profile !== profile ||
        result.converted_segments !== request.left_key_ids.length ||
        result.inserted_keys !== 31 * result.converted_segments ||
        !Number.isFinite(result.error_bound) ||
        result.error_bound < 0 ||
        result.error_bound > value ||
        !Number.isFinite(result.max_absolute_error) ||
        result.max_absolute_error < 0 ||
        result.max_absolute_error > result.error_bound ||
        result.plan.project_id !== project ||
        result.plan.expected_revision !== revision ||
        result.plan.idempotency_key !== request.idempotency_key ||
        result.plan.label !== request.label ||
        !result.plan.operations.length ||
        result.plan.operations.some(
          (op) => !["element_put", "track_put"].includes(op.type),
        )
      )
        throw new Error(
          text(
            "The conversion result differs from the reviewed request.",
            "转换结果与本次审查请求不一致。",
          ),
        );
      setReview({ result, fence });
    } catch (e) {
      if (live.current && epoch.current === serial) setError(message(e));
    } finally {
      flight.current = false;
      if (live.current) setPending(false);
    }
  }
  async function save() {
    if (!review || disabled || flight.current || !scene) return;
    flight.current = true;
    setPending(true);
    setError(null);
    try {
      review.fence();
      await controller.scene.applyPlanned(review.result.plan, scene.sequenceId);
      if (live.current) setReview(null);
    } catch (e) {
      if (live.current) setError(message(e));
    } finally {
      flight.current = false;
      if (live.current) setPending(false);
    }
  }
  if (!legacy.length) return null;
  return (
    <section
      aria-label={text("Legacy lookup conversion", "旧 lookup 曲线转换")}
    >
      <p>
        {text(
          "Legacy curves use 32 linear lookup intervals and decimal constants with 12 places. Convert explicitly before trimming through them. Original keys remain; 31 keys are inserted per segment. This is a bounded approximation.",
          "旧曲线使用 32 段线性查表和 12 位小数常量。若需在曲线中间裁剪，请先显式转换；保留原关键帧，每段插入 31 个关键帧。这是有误差上界的近似转换。",
        )}
      </p>
      <fieldset disabled={disabled || pending}>
        <legend>
          {text("Legacy segments to convert", "选择要转换的旧曲线段")}
        </legend>
        {legacy.map((key) => (
          <label key={key.id}>
            <input
              type="checkbox"
              checked={chosen.has(key.id)}
              onChange={(event) => {
                epoch.current++;
                setReview(null);
                setChosen((old) => {
                  const next = new Set(old);
                  if (event.target.checked) next.add(key.id);
                  else next.delete(key.id);
                  return next;
                });
              }}
            />
            {text("From", "起点")}{" "}
            {(Number(key.time.ticks) / key.time.timebase).toPrecision(6)} s
          </label>
        ))}
      </fieldset>
      {boundary && (
        <p role="status">
          {text(
            "Linear extrapolation at the channel boundary would change when keys are inserted. Explicitly choose Hold before converting.",
            "边界的线性外推会因插入关键帧而改变。请先显式改为保持，再进行转换。",
          )}
          <button
            type="button"
            disabled={disabled || pending}
            onClick={() =>
              onChooseHold(
                {
                  ...channel,
                  extrapolation: { before: "hold", after: "hold" },
                },
                owner,
              )
            }
          >
            {text("Set extrapolation to Hold", "将外推改为保持")}
          </button>
        </p>
      )}
      <label>
        {text(
          "Maximum absolute error (property units)",
          "最大绝对误差（属性单位）",
        )}
        <input
          type="number"
          min="0"
          step="any"
          value={tolerance}
          disabled={disabled || pending}
          onChange={(e) => {
            epoch.current++;
            setReview(null);
            setTolerance(e.target.value);
          }}
        />
      </label>
      <button
        type="button"
        disabled={
          !available || disabled || pending || boundary || chosenIds === "[]"
        }
        onClick={() => void prepare()}
      >
        {text("Review conversion", "审查转换方案")}
      </button>
      {!available && (
        <p>
          {text(
            "This host does not advertise legacy conversion.",
            "当前宿主尚未提供旧曲线转换入口。",
          )}
        </p>
      )}
      {review && (
        <div role="status">
          <p>
            {text("Segments / inserted keys", "转换段数 / 新增关键帧")}:{" "}
            {review.result.converted_segments} / {review.result.inserted_keys}
          </p>
          <p>
            {text(
              "One-sided boundary error / conservative bound",
              "单侧边界误差 / 保守误差上界",
            )}
            : {review.result.max_absolute_error.toExponential(6)} /{" "}
            {review.result.error_bound.toExponential(6)}
          </p>
          <button
            type="button"
            disabled={disabled || pending}
            onClick={() => void save()}
          >
            {text("Apply reviewed conversion", "应用已审查的转换")}
          </button>
        </div>
      )}
      {error && <p role="alert">{error}</p>}
    </section>
  );
}
