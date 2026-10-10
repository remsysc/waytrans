"""
Smart dialogue layout, text clustering, and typography engine for waytrans.
Groups fragmented OCR bounding boxes into coherent dialogue blocks and computes
optimal subtitle positioning and font metrics.
"""

from dataclasses import dataclass
from typing import Dict, List, Optional, Tuple
import re

CJK_REGEX = re.compile(r'[\u3040-\u309f\u30a0-\u30ff\u4e00-\u9faf]')


@dataclass
class TextLine:
    text: str
    left: int
    top: int
    width: int
    height: int

    @property
    def right(self) -> int:
        return self.left + self.width

    @property
    def bottom(self) -> int:
        return self.top + self.height


@dataclass
class DialogueBlock:
    """
    Represents a unified dialogue box or speech bubble composed of one or more text lines.
    """
    lines: List[TextLine]
    left: int
    top: int
    right: int
    bottom: int
    combined_text: str
    avg_line_height: int

    @property
    def width(self) -> int:
        return max(1, self.right - self.left)

    @property
    def height(self) -> int:
        return max(1, self.bottom - self.top)

    @property
    def center_x(self) -> int:
        return self.left + (self.width // 2)


def cluster_text_lines(
    raw_boxes: List[Dict[str, any]],
    max_vertical_gap_ratio: float = 1.8,
    max_horizontal_gap: int = 140
) -> List[DialogueBlock]:
    """
    Clusters nearby, vertically or horizontally adjacent OCR text fragments into coherent
    dialogue blocks using proximity-based spatial union.

    Args:
        raw_boxes: List of bounding box dicts from Tesseract TSV.
        max_vertical_gap_ratio: Maximum vertical distance between consecutive lines
                                relative to average line height.
        max_horizontal_gap: Maximum horizontal separation between fragments on the same line.

    Returns:
        List of merged DialogueBlock objects, ordered top-to-bottom.
    """
    if not raw_boxes:
        return []

    lines = [
        TextLine(
            text=b["text"].strip(),
            left=b["left"],
            top=b["top"],
            width=b["width"],
            height=b["height"]
        )
        for b in raw_boxes if b["text"].strip()
    ]

    if not lines:
        return []

    # Sort lines primarily top-to-bottom, secondarily left-to-right
    lines.sort(key=lambda l: (l.top, l.left))

    clusters: List[List[TextLine]] = []

    for line in lines:
        assigned = False
        for cluster in clusters:
            # Check proximity to any existing line in the cluster
            for member in cluster:
                # Vertical alignment test (consecutive lines in a dialogue paragraph)
                vert_dist = line.top - member.bottom
                vert_overlap = min(line.bottom, member.bottom) - max(line.top, member.top)
                horiz_overlap = min(line.right, member.right) - max(line.left, member.left)
                horiz_dist = max(line.left - member.right, member.left - line.right)

                avg_h = (line.height + member.height) / 2.0
                max_v_gap = avg_h * max_vertical_gap_ratio

                # 1. New line right beneath member (with horizontal overlap or close alignment)
                if 0 <= vert_dist <= max_v_gap and (horiz_overlap > -80 or horiz_dist < max_horizontal_gap):
                    cluster.append(line)
                    assigned = True
                    break

                # 2. On the same horizontal line (fragmented word or dialogue prefix)
                if vert_overlap > (avg_h * 0.4) and horiz_dist <= max_horizontal_gap:
                    cluster.append(line)
                    assigned = True
                    break

            if assigned:
                break

        if not assigned:
            clusters.append([line])

    # Convert clusters to DialogueBlocks
    dialogue_blocks: List[DialogueBlock] = []
    for cluster in clusters:
        min_left = min(l.left for l in cluster)
        min_top = min(l.top for l in cluster)
        max_right = max(l.right for l in cluster)
        max_bottom = max(l.bottom for l in cluster)
        avg_h = int(sum(l.height for l in cluster) / len(cluster))

        # Join text lines smoothly
        # For Japanese text, combine directly without extra whitespace
        combined = "".join(l.text for l in cluster)

        dialogue_blocks.append(
            DialogueBlock(
                lines=cluster,
                left=min_left,
                top=min_top,
                right=max_right,
                bottom=max_bottom,
                combined_text=combined,
                avg_line_height=max(avg_h, 16)
            )
        )

    return dialogue_blocks


def compute_smart_placement(
    block: DialogueBlock,
    translated_text: str,
    viewport_width: int,
    viewport_height: int,
    scan_offset_x: int = 0,
    scan_offset_y: int = 0
) -> Tuple[int, int, int, int]:
    """
    Computes optimal coordinate placement and font sizing for a translated subtitle.

    Placement rules:
    1. Center the English subtitle over the dialogue block horizontally.
    2. Float the subtitle directly above the dialogue block (`block.top - height - margin`).
       If the block is too close to the top of the screen (e.g. nameplate / title), float beneath.
    3. Scale font size smoothly based on original Japanese line height.
    4. Constrain width so text wraps cleanly without spilling off-screen.

    Returns:
        (pos_x, pos_y, font_size_px, max_width_px)
    """
    # 1. Proportional font scaling
    # English subtitles read best when font height is ~85% of original Japanese glyph height
    base_size = int(block.avg_line_height * 0.85)
    font_size = max(13, min(base_size, 26))

    # 2. Maximum width allocation
    # Allocate at least the width of the dialogue box, with a minimum reading width of 320px
    estimated_char_width = font_size * 0.58
    text_length = len(translated_text)
    natural_width = int(text_length * estimated_char_width)

    max_width = max(block.width + 40, min(natural_width, 700))
    max_width = max(max_width, 280)
    max_width = min(max_width, viewport_width - 40)

    # 3. Horizontal Centering
    abs_center_x = scan_offset_x + block.center_x
    pos_x = abs_center_x - (max_width // 2)
    # Clamp within screen bounds with 20px padding
    pos_x = max(20, min(pos_x, viewport_width - max_width - 20))

    # 4. Vertical Placement (Subtitle floating above)
    # Estimate subtitle height based on line wrap
    lines_count = max(1, (natural_width // max_width) + 1)
    estimated_subtitle_h = int(lines_count * font_size * 1.35) + 10

    abs_top = scan_offset_y + block.top
    abs_bottom = scan_offset_y + block.bottom

    margin = 8
    target_y = abs_top - estimated_subtitle_h - margin

    # If too close to the top edge of screen, place immediately below dialogue instead
    if target_y < 10:
        target_y = abs_bottom + margin

    pos_y = max(10, min(target_y, viewport_height - estimated_subtitle_h - 10))

    return pos_x, pos_y, font_size, max_width
