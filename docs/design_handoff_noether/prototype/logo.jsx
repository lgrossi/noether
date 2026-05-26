/* global React */

// Noether logomark
// Concept: Noether's theorem — every symmetry has a conservation law.
// Mark: a rotated square (the "rule") intersected by a horizontal line
// (decisions flowing through). The orange dot marks the moment of decision.

function NoetherMark({ size = 22, color = "currentColor", accent = "#c2410c" }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 22 22"
      fill="none"
      aria-hidden="true"
      style={{ display: "block" }}
    >
      <rect
        x="5.5"
        y="5.5"
        width="11"
        height="11"
        transform="rotate(45 11 11)"
        stroke={color}
        strokeWidth="1.4"
        fill="none"
      />
      <line x1="1.5" y1="11" x2="20.5" y2="11" stroke={color} strokeWidth="1.4" strokeLinecap="round" />
      <circle cx="11" cy="11" r="2.2" fill={accent} />
    </svg>
  );
}

function NoetherLogo({ size = 22, color, accent, showWord = true }) {
  return (
    <span
      className="brand"
      style={{ display: "inline-flex", alignItems: "center", gap: 8 }}
    >
      <NoetherMark size={size} color={color} accent={accent} />
      {showWord && (
        <span
          style={{
            fontFamily: '"Newsreader", serif',
            fontStyle: "italic",
            fontWeight: 500,
            fontSize: size * 0.95,
            letterSpacing: "-0.02em",
            color: "var(--ink)",
            lineHeight: 1,
          }}
        >
          noether
        </span>
      )}
    </span>
  );
}

window.NoetherLogo = NoetherLogo;
window.NoetherMark = NoetherMark;
