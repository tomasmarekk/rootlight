% Native syntax fixture: nested/local functions and source-preserving expressions.
% The Unicode text exercises byte coordinates, not identifier extensions.
function [result, count] = summarize(values, scale)
arguments
    values (1,:) double
    scale (1,1) double = 2
end
label = "λ😀 function hidden(value)";
quoted = 'It''s a function fake(x)';
matrix = [1, 2; 3, 4];
transposed = matrix';
plain = matrix.';
cells = {label, quoted};
transform = @(item) item * scale;
result = transform(values(1)) + ...
    adjust(scale);
count = numel(values);
    function output = adjust(input)
        output = input + 1;
    end
end

function output = identity(input, ~)
output = input;
end
