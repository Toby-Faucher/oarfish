<script lang="ts">
  /**
   * Optional ambient water shader, fixed behind the app chrome (see the
   * `-z-10` wrapper). Off by default; toggled from Settings → Appearance.
   *
   * Deliberate exception to the app's usual "two moments, total" motion
   * budget — see CLAUDE.md's motion section. Kept subtle (low opacity,
   * slow speed) so it reads as ambient depth, not something watched.
   *
   * Uses the vanilla `@paper-design/shaders` ShaderMount directly rather
   * than a framework wrapper package, mirroring how their React `Water`
   * component builds uniforms (there's no official Svelte wrapper).
   */
  import {
    ShaderMount,
    waterFragmentShader,
    getShaderColorFromString,
    defaultObjectSizing,
    ShaderFitOptions,
    emptyPixel,
  } from '@paper-design/shaders';
  import { currentTheme } from '../lib/theme.svelte';
  import { shaderBgEnabled } from '../lib/shaderBg.svelte';

  let container: HTMLDivElement | undefined = $state();
  let mount: ShaderMount | null = null;

  const reducedMotion =
    typeof window !== 'undefined' &&
    window.matchMedia('(prefers-reduced-motion: reduce)').matches;

  // The Water shader samples `u_image` even in standalone (no-photo) mode —
  // an unbound sampler fails WebGL's draw-call validation outright, so
  // nothing renders at all rather than just missing a texture. This 1x1
  // transparent pixel satisfies the binding without adding visible content.
  let emptyPixelImage: Promise<HTMLImageElement> | undefined;
  function loadEmptyPixel(): Promise<HTMLImageElement> {
    if (!emptyPixelImage) {
      emptyPixelImage = new Promise((resolve, reject) => {
        const img = new Image();
        img.onload = () => resolve(img);
        img.onerror = () => reject(new Error('ocean background: empty pixel failed to load'));
        img.src = emptyPixel;
      });
    }
    return emptyPixelImage;
  }

  function waterColors() {
    const style = getComputedStyle(document.documentElement);
    return {
      colorBack: style.getPropertyValue('--ui-water').trim(),
      colorHighlight: style.getPropertyValue('--ui-water-highlight').trim(),
    };
  }

  function buildUniforms(colorBack: string, colorHighlight: string, image: HTMLImageElement) {
    return {
      u_image: image,
      u_colorBack: getShaderColorFromString(colorBack),
      u_colorHighlight: getShaderColorFromString(colorHighlight),
      u_highlights: 0.28,
      u_layering: 0.3,
      u_waves: 0.25,
      u_edges: 0.8,
      u_caustic: 0,
      u_size: 0.9,
      u_fit: ShaderFitOptions[defaultObjectSizing.fit],
      u_rotation: defaultObjectSizing.rotation,
      u_scale: 1,
      u_offsetX: defaultObjectSizing.offsetX,
      u_offsetY: defaultObjectSizing.offsetY,
      u_originX: defaultObjectSizing.originX,
      u_originY: defaultObjectSizing.originY,
      u_worldWidth: defaultObjectSizing.worldWidth,
      u_worldHeight: defaultObjectSizing.worldHeight,
    };
  }

  // Mount/dispose as the toggle flips. `speed: 0` when the OS asks for
  // reduced motion — ShaderMount stops its render loop entirely rather
  // than just holding still, so it's a real zero, not a paused cost.
  $effect(() => {
    const enabled = shaderBgEnabled();
    const target = container;
    if (!enabled || !target) {
      mount?.dispose();
      mount = null;
      return;
    }

    let cancelled = false;
    loadEmptyPixel()
      .then((image) => {
        if (cancelled) return;
        const { colorBack, colorHighlight } = waterColors();
        try {
          mount = new ShaderMount(
            target,
            waterFragmentShader,
            buildUniforms(colorBack, colorHighlight, image),
            undefined,
            reducedMotion ? 0 : 0.12,
          );
        } catch {
          // No WebGL2 in this browser — leave the (empty) container as-is.
          mount = null;
        }
      })
      .catch(() => {});

    return () => {
      cancelled = true;
      mount?.dispose();
      mount = null;
    };
  });

  // Re-tint on theme switches without a full remount.
  $effect(() => {
    currentTheme();
    if (!mount) return;
    const { colorBack, colorHighlight } = waterColors();
    mount.setUniforms({
      u_colorBack: getShaderColorFromString(colorBack),
      u_colorHighlight: getShaderColorFromString(colorHighlight),
    });
  });
</script>

<div
  bind:this={container}
  class="pointer-events-none fixed inset-0 -z-10 opacity-50"
  aria-hidden="true"
></div>
