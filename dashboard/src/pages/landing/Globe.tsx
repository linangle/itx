import { useEffect, useRef } from "react";
import * as THREE from "three";

/** Landing red, shared with landing.css. */
const RED = "#d8402d";
const GLOBE_RADIUS = 1.55;

/** Axial lean, radians -- Earth's ~23.5°, with the north pole tipping
 * to the upper LEFT and the south pole exiting lower right, matching
 * the axis diagram the user supplied. Positive rotation.z tips the
 * +Y pole toward -X (screen left). */
const AXIAL_TILT = 0.41;

/** Where the "sun" sits for the cel shade: upper right and slightly in
 * front, so the hard shadow crescent falls on the lower left exactly
 * where the reference mock draws it. This is a shader uniform, not a
 * scene light -- the scene has no lights at all, because the look is
 * flat 2D animation, not 3D rendering. */
const LIGHT_DIR = new THREE.Vector3(0.55, 0.4, 0.55).normalize();

interface OrbitSpec {
  /** Orbit radius in world units (the globe itself is 1.55). */
  radius: number;
  /** Angular speed in rad/s; the sign sets the direction. Every value
   * is well above the globe's 0.14 rad/s spin -- satellites overtake
   * the surface, per the user's spec. */
  speed: number;
  /** Starting angle, spread so the swarm stays distributed. */
  phase: number;
  /** Orbit-plane lean toward/away from the camera. */
  tiltX: number;
  /** Rotation of the plane about the vertical axis. Combined with a
   * non-zero `tiltX` this is what actually points each orbit's normal
   * somewhere different -- without it every ring shares an axis and
   * the satellites swing behind and in front of the globe together. */
  swivel: number;
  /** Orbit-plane lean across the screen, on top of the global tilt. */
  tiltZ: number;
  /** Orb radius. */
  size: number;
  /** How far back the trail arcs along the orbit, radians. Kept short
   * enough that a comet plus its whole tail fits in the frame. */
  sweep: number;
}

/** Six satellites on deliberately mixed planes and directions. Radii
 * stay under 2.45 so every orbit (plus orb size) fits inside the
 * camera frustum: nothing gets clipped mid-trail at the canvas edge.
 *
 * Two things keep the swarm from bunching up. Each plane gets its own
 * `swivel`, so the orbits genuinely face different ways rather than
 * being one ring seen six times; and the speeds are mutually
 * non-commensurate (no value is a near-multiple of another), so
 * whatever alignment happens to occur doesn't recur on a short
 * period. Phases start evenly spread around the circle. */
const ORBITS: OrbitSpec[] = [
  { radius: 2.1, speed: 1.13, phase: 0.0, tiltX: 0.5, swivel: 0.0, tiltZ: -0.28, size: 0.105, sweep: 1.05 },
  { radius: 1.86, speed: -0.94, phase: 1.05, tiltX: -0.62, swivel: 1.1, tiltZ: 0.34, size: 0.085, sweep: 0.95 },
  { radius: 2.02, speed: 1.37, phase: 2.09, tiltX: 0.22, swivel: 2.3, tiltZ: 0.48, size: 0.115, sweep: 1.15 },
  { radius: 1.94, speed: -1.21, phase: 3.14, tiltX: -0.3, swivel: 3.5, tiltZ: -0.42, size: 0.075, sweep: 0.85 },
  { radius: 2.07, speed: 0.86, phase: 4.19, tiltX: 0.68, swivel: 4.6, tiltZ: 0.12, size: 0.095, sweep: 1.0 },
  { radius: 1.8, speed: -1.49, phase: 5.24, tiltX: -0.44, swivel: 5.7, tiltZ: -0.16, size: 0.1, sweep: 1.1 },
];

/** Every drawn thing sits within this radius: the widest orbit, plus
 * that comet's orb, plus its glow sprite's half-extent. Checked at
 * startup against the camera frustum so a satellite can never be cut
 * off at the canvas edge -- the failure the eye reads as the globe
 * "clipping into" the text column beside it. */
const MAX_EXTENT = Math.max(...ORBITS.map((o) => o.radius + o.size * (1 + 2.4 / 2)));

/** Cel-shaded globe: the texture at full brightness on the lit side, a
 * single flat blue-grey multiply on the shaded side, and a genuinely
 * hard edge between them. `colorspace_fragment` keeps the output
 * identical to how the built-in materials would encode the same
 * colors. */
function makeGlobeMaterial(texture: THREE.Texture): THREE.ShaderMaterial {
  return new THREE.ShaderMaterial({
    uniforms: {
      map: { value: texture },
      lightDir: { value: LIGHT_DIR },
      shadowTint: { value: new THREE.Color(0.34, 0.38, 0.68) },
      /* Sets how much of the disc falls in shade. Positive values push
       * the terminator across the middle of the globe (+0.3 put half of
       * it in shade, which read as gloom); far-negative ones squeeze it
       * out to the rim as a sliver (-0.35 was barely visible). Just
       * under zero puts the cut near the light's great circle, giving
       * the broad lower-left crescent the mock draws. */
      threshold: { value: -0.05 },
    },
    vertexShader: /* glsl */ `
      varying vec2 vUv;
      varying vec3 vNormal;
      void main() {
        vUv = uv;
        vNormal = normalize(mat3(modelMatrix) * normal);
        gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0);
      }
    `,
    fragmentShader: /* glsl */ `
      uniform sampler2D map;
      uniform vec3 lightDir;
      uniform vec3 shadowTint;
      uniform float threshold;
      varying vec2 vUv;
      varying vec3 vNormal;
      void main() {
        vec4 tex = texture2D(map, vUv);
        float d = dot(normalize(vNormal), lightDir) - threshold;
        // The terminator is a hard cut, blended only across the width
        // of a single pixel. Using fwidth (how fast d changes between
        // neighbouring fragments) rather than a fixed softness is what
        // keeps it crisp: a constant ramp covers more screen pixels the
        // larger the globe is drawn, which is what read as blur.
        float aa = fwidth(d);
        float lit = smoothstep(-aa, aa, d);
        gl_FragColor = vec4(tex.rgb * mix(shadowTint, vec3(1.0), lit), 1.0);
        #include <colorspace_fragment>
      }
    `,
  });
}

/** Soft radial red blob behind each orb -- the hand-drawn bloom in the
 * mock, drawn once onto a 2D canvas. */
function makeGlowTexture(): THREE.CanvasTexture {
  const size = 128;
  const canvas = document.createElement("canvas");
  canvas.width = canvas.height = size;
  const ctx = canvas.getContext("2d")!;
  const g = ctx.createRadialGradient(64, 64, 0, 64, 64, 64);
  g.addColorStop(0, "rgba(216, 64, 45, 0.9)");
  g.addColorStop(0.35, "rgba(216, 64, 45, 0.45)");
  g.addColorStop(1, "rgba(216, 64, 45, 0)");
  ctx.fillStyle = g;
  ctx.fillRect(0, 0, size, size);
  const tex = new THREE.CanvasTexture(canvas);
  tex.colorSpace = THREE.SRGBColorSpace;
  return tex;
}

const TRAIL_TUBULAR = 64;
const TRAIL_RADIAL = 8;

/** Shrink each cross-section ring of a tube toward its own center, so
 * the trail starts at the orb's full thickness and thins toward the
 * tail -- the comet shape the user drew, rather than a ball towing a
 * skinnier pipe. Ring centers are recovered by averaging the ring's
 * vertices, which sidesteps re-deriving the curve's Frenet frames. */
function taperTube(geometry: THREE.TubeGeometry): void {
  const pos = geometry.attributes.position;
  const rings = TRAIL_TUBULAR + 1;
  const ringVerts = TRAIL_RADIAL + 1;
  for (let i = 0; i < rings; i++) {
    let cx = 0;
    let cy = 0;
    let cz = 0;
    for (let j = 0; j < ringVerts; j++) {
      const k = i * ringVerts + j;
      cx += pos.getX(k);
      cy += pos.getY(k);
      cz += pos.getZ(k);
    }
    cx /= ringVerts;
    cy /= ringVerts;
    cz /= ringVerts;
    const s = i / (rings - 1);
    const f = 1 - 0.5 * s;
    for (let j = 0; j < ringVerts; j++) {
      const k = i * ringVerts + j;
      pos.setXYZ(
        k,
        cx + (pos.getX(k) - cx) * f,
        cy + (pos.getY(k) - cy) * f,
        cz + (pos.getZ(k) - cz) * f,
      );
    }
  }
  pos.needsUpdate = true;
}

/** The trail is a tapered tube along the arc the orb just travelled,
 * fading out along its length in the shader. The arc is *static* in
 * the spinner group's frame: the whole group rotates each frame, so
 * the tube always trails the orb exactly without geometry updates. A
 * point placed at local angle phi sits at world angle (rotation.y +
 * phi), and the orb's past positions are at smaller world angles when
 * speed > 0 -- hence phi runs from 0 back to -sign(speed) * sweep.
 *
 * The tube's head radius matches the orb and its head alpha is nearly
 * opaque, so orb and trail read as one continuous 2D comet instead of
 * a sphere with an attachment. */
function makeTrail(spec: OrbitSpec): THREE.Mesh {
  const points: THREE.Vector3[] = [];
  const STEPS = 48;
  for (let i = 0; i <= STEPS; i++) {
    const phi = -Math.sign(spec.speed) * (i / STEPS) * spec.sweep;
    points.push(
      new THREE.Vector3(spec.radius * Math.cos(phi), 0, -spec.radius * Math.sin(phi)),
    );
  }
  const curve = new THREE.CatmullRomCurve3(points);
  const geometry = new THREE.TubeGeometry(curve, TRAIL_TUBULAR, spec.size, TRAIL_RADIAL, false);
  taperTube(geometry);
  const material = new THREE.ShaderMaterial({
    transparent: true,
    depthWrite: false,
    uniforms: { color: { value: new THREE.Color(RED) } },
    vertexShader: /* glsl */ `
      varying float vAlong;
      void main() {
        vAlong = uv.x;
        gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0);
      }
    `,
    fragmentShader: /* glsl */ `
      uniform vec3 color;
      varying float vAlong;
      void main() {
        // Exponent below 1 holds the trail near full opacity for most
        // of its length and drops off only at the tail. At a linear or
        // steeper fade the mid-trail sits around 30-40% alpha, and red
        // at that alpha over the light-blue ocean composites to a
        // grey-lavender -- which passes across the globe as a smudge
        // and reads as the sphere being blurry.
        gl_FragColor = vec4(color, pow(1.0 - vAlong, 0.55));
        #include <colorspace_fragment>
      }
    `,
  });
  const mesh = new THREE.Mesh(geometry, material);
  mesh.renderOrder = 1;
  return mesh;
}

/** The hero's scene: the cel-shaded globe spinning west->east on the
 * diagram's tilted axis, four red comets riding its equatorial band.
 * Purely decorative, so the mount is aria-hidden and a machine with no
 * WebGL simply gets an empty column instead of an error. */
export default function Globe() {
  const mountRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    const mount = mountRef.current;
    if (!mount) return;

    let renderer: THREE.WebGLRenderer;
    try {
      renderer = new THREE.WebGLRenderer({ antialias: true, alpha: true });
    } catch {
      return;
    }
    renderer.setPixelRatio(Math.min(window.devicePixelRatio, 2));
    mount.appendChild(renderer.domElement);

    const scene = new THREE.Scene();
    const camera = new THREE.PerspectiveCamera(40, 1, 0.1, 50);
    camera.position.set(0, 0, 8);

    // Deferred single-frame render for the reduced-motion path, where no
    // animation loop will repaint once the texture finishes loading.
    let onTextureReady = () => {};
    const texture = new THREE.TextureLoader().load(
      `${import.meta.env.BASE_URL}globe-texture.png`,
      () => onTextureReady(),
    );
    texture.colorSpace = THREE.SRGBColorSpace;
    texture.anisotropy = 4;

    const assembly = new THREE.Group();
    assembly.rotation.z = AXIAL_TILT;
    scene.add(assembly);

    const globe = new THREE.Mesh(
      new THREE.SphereGeometry(GLOBE_RADIUS, 96, 96),
      makeGlobeMaterial(texture),
    );
    assembly.add(globe);

    const glowTexture = makeGlowTexture();
    const spinners: { group: THREE.Group; spec: OrbitSpec }[] = [];
    for (const spec of ORBITS) {
      const plane = new THREE.Group();
      // YXZ so `swivel` turns the already-tilted ring about the world
      // vertical; with the default XYZ order it would just re-phase an
      // untilted ring and every orbit would still share a normal.
      plane.rotation.order = "YXZ";
      plane.rotation.set(spec.tiltX, spec.swivel, spec.tiltZ);
      const spinner = new THREE.Group();
      spinner.rotation.y = spec.phase;

      const orb = new THREE.Mesh(
        new THREE.SphereGeometry(spec.size, 32, 32),
        new THREE.MeshBasicMaterial({ color: RED }),
      );
      orb.position.set(spec.radius, 0, 0);

      // Kept tight and faint. A large soft sprite is the one element
      // here that genuinely blurs: when a comet passes in front of the
      // globe its halo washes over the map and the whole sphere reads
      // as low-res, however hard the terminator itself is.
      const glow = new THREE.Sprite(
        new THREE.SpriteMaterial({
          map: glowTexture,
          transparent: true,
          depthWrite: false,
          opacity: 0.55,
        }),
      );
      glow.position.copy(orb.position);
      glow.scale.setScalar(spec.size * 2.4);
      glow.renderOrder = 2;

      spinner.add(makeTrail(spec), orb, glow);
      plane.add(spinner);
      assembly.add(plane);
      spinners.push({ group: spinner, spec });
    }

    const resize = () => {
      const { clientWidth: w, clientHeight: h } = mount;
      if (w === 0 || h === 0) return;
      renderer.setSize(w, h, false);
      camera.aspect = w / h;

      // Pull back exactly far enough that MAX_EXTENT fits, with a 6%
      // margin -- and no further, so the globe fills as much of the
      // box as it safely can. Deriving this instead of hard-coding a
      // distance means the orbits can be retuned without silently
      // reintroducing clipping at the canvas edge. The narrower of the
      // two half-angles governs, which on a portrait box is the
      // horizontal one.
      const halfV = Math.tan(THREE.MathUtils.degToRad(camera.fov) / 2);
      const halfH = halfV * Math.min(1, camera.aspect);
      camera.position.z = (MAX_EXTENT * 1.06) / Math.min(halfV, halfH);

      camera.updateProjectionMatrix();
    };
    resize();
    const observer = new ResizeObserver(() => {
      resize();
      if (reduceMotion) renderFrame();
    });
    observer.observe(mount);

    const reduceMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    const clock = new THREE.Clock();
    let raf = 0;
    // Accumulated from per-frame deltas rather than read from the clock's
    // own elapsed time, so the loop can stop and restart without the
    // scene jumping: time spent paused simply never gets added.
    let elapsed = 0;

    const renderFrame = () => {
      elapsed += clock.getDelta();
      // Positive rotation.y spins the front face left-to-right on
      // screen: west->east, the diagram's "direction of spin".
      globe.rotation.y = elapsed * 0.14;
      for (const { group, spec } of spinners) {
        group.rotation.y = spec.phase + elapsed * spec.speed;
      }
      renderer.render(scene, camera);
    };

    // Only runs while the globe is actually on screen. This is the most
    // expensive thing on the page -- a WebGL scene redrawing every frame
    // -- and once you have scrolled to the board it is drawing to nobody.
    // The board polls the hub on a timer, so those two were competing for
    // the same main thread for no reason.
    const visibility = new IntersectionObserver(
      ([entry]) => {
        if (entry.isIntersecting) start();
        else stop();
      },
      { threshold: 0 },
    );

    const loop = () => {
      renderFrame();
      raf = requestAnimationFrame(loop);
    };

    const start = () => {
      if (raf !== 0) return;
      // Throw away the gap spent paused; otherwise the first delta after
      // resuming is however long you were reading the board, and the
      // globe would snap forward by it.
      clock.getDelta();
      raf = requestAnimationFrame(loop);
    };

    const stop = () => {
      if (raf === 0) return;
      cancelAnimationFrame(raf);
      raf = 0;
    };

    if (reduceMotion) {
      clock.stop();
      onTextureReady = renderFrame;
      renderFrame();
    } else {
      visibility.observe(mount);
    }

    return () => {
      cancelAnimationFrame(raf);
      visibility.disconnect();
      observer.disconnect();
      scene.traverse((obj) => {
        if (obj instanceof THREE.Mesh) {
          obj.geometry.dispose();
          (obj.material as THREE.Material).dispose();
        } else if (obj instanceof THREE.Sprite) {
          obj.material.dispose();
        }
      });
      texture.dispose();
      glowTexture.dispose();
      renderer.dispose();
      mount.removeChild(renderer.domElement);
    };
  }, []);

  return <div ref={mountRef} className="itx-globe-mount" aria-hidden="true" />;
}
